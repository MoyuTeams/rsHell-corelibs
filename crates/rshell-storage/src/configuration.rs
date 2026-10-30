use std::collections::BTreeSet;

use rshell_core::{
    AppSettings, TerminalProfile, TerminalProfileId, validate_app_settings,
    validate_terminal_profile,
};
use rusqlite::{Connection, params};

use crate::{StorageError, error, mapping, profiles, transaction, worker::FailureInjector};

/// 同一数据库读事务中的完整设置、终端配置及持久版本号。
///
/// 版本号仅覆盖设置与终端配置；连接目录和凭据不属于此快照。
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigurationSnapshot {
    pub revision: u64,
    pub settings: AppSettings,
    pub profiles: Vec<TerminalProfile>,
}

/// 一次原子提交：完整替换设置，按 ID 创建或更新配置，并删除指定配置。
///
/// 未列出的配置保持不变。同一 ID 不得重复或同时出现在保存和删除列表中；
/// 删除不存在、提交前默认或被连接引用的配置将返回 `StorageError::Constraint`。
/// 切换默认配置与删除原默认配置必须分为两次提交。
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigurationChange {
    pub expected_revision: u64,
    pub settings: AppSettings,
    pub upsert_profiles: Vec<TerminalProfile>,
    pub delete_profiles: Vec<TerminalProfileId>,
}

/// 冲突不是存储故障；调用方应重新读取快照后让用户确认或合并修改。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "必须检查提交成功或版本冲突，不能将 Ok 一律视为保存成功"]
pub enum ConfigurationCommitOutcome {
    /// 每次成功提交（包括内容相同的提交）恰好递增一次版本号。
    Applied { revision: u64 },
    /// 版本不匹配时不执行任何写入，且优先于内容校验返回。
    Conflict { actual_revision: u64 },
}

pub(crate) fn load(connection: &mut Connection) -> Result<ConfigurationSnapshot, StorageError> {
    // 显式读事务避免其他 worker 在多个 SELECT 之间提交，造成混合版本快照。
    let transaction = connection.transaction().map_err(error::sqlite)?;
    let snapshot = ConfigurationSnapshot {
        revision: revision(&transaction)?,
        settings: profiles::load_settings(&transaction)?,
        profiles: profiles::load_profiles(&transaction)?,
    };
    transaction.commit().map_err(error::sqlite)?;
    Ok(snapshot)
}

pub(crate) fn commit(
    connection: &mut Connection,
    failure: &mut FailureInjector,
    change: ConfigurationChange,
) -> Result<ConfigurationCommitOutcome, StorageError> {
    transaction::immediate(connection, |transaction| {
        let actual_revision = revision(transaction)?;
        if actual_revision != change.expected_revision {
            return Ok(ConfigurationCommitOutcome::Conflict { actual_revision });
        }

        let mut ids = BTreeSet::new();
        for profile in &change.upsert_profiles {
            if !ids.insert(profile.id) {
                return Err(StorageError::Constraint);
            }
            validate_terminal_profile(profile).map_err(|_| StorageError::Constraint)?;
        }
        let previous_settings = profiles::load_settings(transaction)?;
        for id in &change.delete_profiles {
            if !ids.insert(*id) || *id == previous_settings.default_terminal_profile {
                return Err(StorageError::Constraint);
            }
        }

        for profile in change.upsert_profiles {
            profiles::write_profile(transaction, profile)?;
            failure.after_statement()?;
        }
        // 既有外键保护连接引用；默认配置还由提交前检查与设置校验共同保护。
        for id in change.delete_profiles {
            let deleted = transaction
                .execute(
                    "DELETE FROM terminal_profiles WHERE id=?1",
                    [mapping::uuid_text(id.0)],
                )
                .map_err(error::sqlite)?;
            if deleted != 1 {
                return Err(StorageError::Constraint);
            }
            failure.after_statement()?;
        }
        validate_app_settings(&change.settings, &profiles::load_profiles(transaction)?)
            .map_err(|_| StorageError::Constraint)?;
        profiles::write_settings(transaction, change.settings)?;
        failure.after_statement()?;
        let revision = advance_revision(transaction)?;
        failure.after_statement()?;
        Ok(ConfigurationCommitOutcome::Applied { revision })
    })
}

fn revision(connection: &Connection) -> Result<u64, StorageError> {
    let value: i64 = connection
        .query_row(
            "SELECT revision FROM configuration_revision WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(error::sqlite)?;
    u64::try_from(value).map_err(|_| StorageError::Corrupt)
}

/// 只能在调用方已持有的写事务内执行；溢出时拒绝提交而不是回绕。
pub(crate) fn advance_revision(connection: &Connection) -> Result<u64, StorageError> {
    let next = revision(connection)? + 1;
    let stored = i64::try_from(next).map_err(|_| StorageError::Constraint)?;
    let updated = connection
        .execute(
            "UPDATE configuration_revision SET revision=?1 WHERE singleton=1",
            params![stored],
        )
        .map_err(error::sqlite)?;
    if updated != 1 {
        return Err(StorageError::Corrupt);
    }
    Ok(next)
}
