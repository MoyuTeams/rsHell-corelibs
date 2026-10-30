use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use rshell_platform::{create_private_file, durable_replace_user_file, harden_private_file};
use russh::keys::{PublicKey, known_hosts};

use super::{HostKeyError, HostKeyStorageStep};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn store(
    destination: &Path,
    host: &str,
    port: u16,
    key: &PublicKey,
) -> Result<(), HostKeyError> {
    store_with_copy(destination, host, port, key, copy_existing_file)
}

/// Learns `key` for `host`:`port` in place of every key recorded for them.
pub(super) fn replace(
    destination: &Path,
    host: &str,
    port: u16,
    key: &PublicKey,
) -> Result<(), HostKeyError> {
    // russh numbers entries from 1, skipping comment lines, the same way `copy_without_endpoint`
    // counts them, so the matched entries map directly onto the lines rewritten below.
    let matched = known_hosts::known_host_keys_path(host, port, destination)
        .map_err(|_| storage_error(host, port, HostKeyStorageStep::CopyExisting))?
        .into_iter()
        .map(|(entry, _)| entry)
        .collect::<BTreeSet<_>>();
    if matched.is_empty() {
        return Err(storage_error(host, port, HostKeyStorageStep::CopyExisting));
    }
    store_with_copy(destination, host, port, key, |path, target| {
        copy_without_endpoint(path, target, host, port, &matched)
    })
}

fn store_with_copy(
    destination: &Path,
    host: &str,
    port: u16,
    key: &PublicKey,
    copy: impl FnOnce(&Path, &mut File) -> io::Result<()>,
) -> Result<(), HostKeyError> {
    let Some(parent) = destination.parent() else {
        return Err(storage_error(host, port, HostKeyStorageStep::CreateParent));
    };
    fs::create_dir_all(parent)
        .map_err(|_| storage_error(host, port, HostKeyStorageStep::CreateParent))?;

    let (temporary_path, temporary_file) = create_private_temporary_file(parent)
        .map_err(|_| storage_error(host, port, HostKeyStorageStep::CreateTemporary))?;
    let temporary = TemporaryKnownHostsFile(temporary_path);
    {
        let mut temporary_file = temporary_file;
        copy(destination, &mut temporary_file)
            .map_err(|_| storage_error(host, port, HostKeyStorageStep::CopyExisting))?;
        terminate_last_record(&mut temporary_file)
            .map_err(|_| storage_error(host, port, HostKeyStorageStep::CopyExisting))?;
        temporary_file
            .flush()
            .and_then(|()| temporary_file.sync_all())
            .map_err(|_| storage_error(host, port, HostKeyStorageStep::SyncTemporary))?;
    }

    known_hosts::learn_known_hosts_path(host, port, key, temporary.path())
        .map_err(|_| storage_error(host, port, HostKeyStorageStep::Learn))?;
    OpenOptions::new()
        .write(true)
        .open(temporary.path())
        .and_then(|file| file.sync_all())
        .map_err(|_| storage_error(host, port, HostKeyStorageStep::SyncLearned))?;
    harden_private_file(temporary.path())
        .map_err(|_| storage_error(host, port, HostKeyStorageStep::HardenTemporary))?;
    durable_replace_user_file(temporary.path(), destination)
        .map_err(|_| storage_error(host, port, HostKeyStorageStep::Replace))?;
    Ok(())
}

// russh appends the learned record. An existing file without a final newline needs a delimiter
// so the new record does not become part of its last (possibly shared) host-key entry.
fn terminate_last_record(file: &mut File) -> io::Result<()> {
    if file.stream_position()? == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0];
    file.read_exact(&mut last)?;
    if last[0] != b'\n' {
        file.write_all(b"\n")?;
    }
    Ok(())
}

fn storage_error(host: &str, port: u16, step: HostKeyStorageStep) -> HostKeyError {
    HostKeyError::storage(host, port, step)
}

fn copy_existing_file(path: &Path, target: &mut File) -> io::Result<()> {
    match File::open(path) {
        Ok(mut source) => {
            io::copy(&mut source, target)?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Copies the known-hosts file, removing only the `host`:`port` endpoint from the entries russh
/// matched (numbered from 1, skipping comments). Other hostnames on a shared line keep their
/// recorded key; a record whose only endpoint was the target is dropped. A matched hashed entry
/// cannot be rewritten token-by-token, so replacing through one fails closed.
fn copy_without_endpoint(
    path: &Path,
    target: &mut File,
    host: &str,
    port: u16,
    matched: &BTreeSet<usize>,
) -> io::Result<()> {
    let content = fs::read(path)?;
    let mut entry = 0;
    let mut rewritten = BTreeSet::new();
    for line in content.split_inclusive(|&byte| byte == b'\n') {
        // russh numbers entries from 1 and skips only lines that begin with '#', so blank
        // lines still advance the counter.
        let is_comment = line.first() == Some(&b'#');
        if !is_comment {
            entry += 1;
        }
        if is_comment || !matched.contains(&entry) {
            target.write_all(line)?;
            continue;
        }
        rewritten.insert(entry);
        match rewritten_endpoint_line(line, host, port) {
            Rewrite::Rewritten(bytes) => target.write_all(&bytes)?,
            Rewrite::Drop => {}
            Rewrite::Unsafe => {
                return Err(io::Error::other(
                    "matched known-hosts endpoint cannot be isolated",
                ));
            }
        }
    }
    if rewritten != *matched {
        return Err(io::Error::other("matched known-hosts entries changed"));
    }
    Ok(())
}

enum Rewrite {
    Rewritten(Vec<u8>),
    Drop,
    Unsafe,
}

fn rewritten_endpoint_line(line: &[u8], host: &str, port: u16) -> Rewrite {
    let Some((hosts, rest)) = split_hosts_field(line) else {
        return Rewrite::Unsafe;
    };
    let endpoint = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    let mut kept = Vec::new();
    let mut removed = false;
    for token in split_comma_list(hosts) {
        if token.is_empty()
            || token.starts_with(b"|1|")
            || token.starts_with(b"!")
            || token.contains(&b'*')
            || token.contains(&b'?')
        {
            // A hashed token may be the target endpoint, but cannot be verified or removed
            // without recomputing its HMAC; patterns are also unsafe to rewrite.
            return Rewrite::Unsafe;
        }
        if token == endpoint.as_bytes() {
            removed = true;
        } else {
            kept.push(token);
        }
    }
    if !removed {
        return Rewrite::Unsafe;
    }
    if kept.is_empty() {
        return Rewrite::Drop;
    }
    let mut kept = kept.join(&b","[..]);
    kept.extend_from_slice(rest);
    Rewrite::Rewritten(kept)
}

fn split_hosts_field(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let separator = line.iter().position(u8::is_ascii_whitespace)?;
    Some(line.split_at(separator))
}

fn split_comma_list(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|byte| *byte == b',')
}

fn create_private_temporary_file(
    parent: &Path,
) -> Result<(std::path::PathBuf, File), rshell_platform::PlatformError> {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = parent.join(format!(
        ".rshell-known-hosts-{}-{sequence}.tmp",
        std::process::id()
    ));
    create_private_file(&path).map(|file| (path, file))
}

struct TemporaryKnownHostsFile(std::path::PathBuf);

impl TemporaryKnownHostsFile {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryKnownHostsFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::parse_public_key_base64;
    use tempfile::TempDir;

    const KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAILagOJFgwaMNhBWQINinKOXmqS4Gh5NgxgriXwdOoINJ";

    #[test]
    fn copy_failure_preserves_destination_and_removes_private_temporary_file() {
        let temp = TempDir::new().unwrap();
        let destination = temp.path().join("known_hosts");
        fs::write(&destination, b"original\n").unwrap();
        let key = parse_public_key_base64(KEY).unwrap();

        let error = store_with_copy(&destination, "storage.test", 22, &key, |_, target| {
            target.write_all(b"partial")?;
            Err(io::Error::other("injected copy failure"))
        })
        .expect_err("copy failure must abort learning");

        assert!(matches!(
            error,
            HostKeyError::Storage {
                step: HostKeyStorageStep::CopyExisting,
                ..
            }
        ));
        assert_eq!(fs::read(&destination).unwrap(), b"original\n");
        let entries = fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, ["known_hosts"]);
    }
}
