#[cfg(feature = "test-support")]
use std::collections::BTreeSet;

use rshell_core::{
    AppSettings, ColorScheme, KeyBinding, KeyCode, KeyModifiers, TerminalProfile,
    TerminalSettingsV1,
};
use rshell_storage::{SqliteRepository, StorageError};

#[test]
fn migration_is_monotonic_idempotent_and_seeds_defaults() {
    let repository = SqliteRepository::open_in_memory().unwrap();
    assert_eq!(repository.schema_versions().unwrap(), Vec::<i64>::new());

    repository.migrate().unwrap();
    repository.migrate().unwrap();

    assert_eq!(repository.schema_versions().unwrap(), vec![1, 2, 3]);
    assert_eq!(
        repository.load_terminal_profiles().unwrap(),
        vec![TerminalProfile::p0_default()]
    );
    assert_eq!(repository.load_settings().unwrap(), AppSettings::default());
    repository.shutdown().unwrap();
}

#[test]
fn profile_and_settings_json_round_trip_all_versioned_fields() {
    let repository = SqliteRepository::open_in_memory().unwrap();
    repository.migrate().unwrap();
    let profile = TerminalProfile {
        name: "Unicode 配置".into(),
        settings: TerminalSettingsV1 {
            terminal_type: "screen-256color".into(),
            initial_cols: 132,
            initial_rows: 44,
            scrollback_lines: 42_000,
            font_family: "Cascadia Mono".into(),
            font_size: 13.5,
            color_scheme: ColorScheme::TokyoNight,
            key_bindings: vec![KeyBinding {
                code: KeyCode::F(6),
                modifiers: KeyModifiers {
                    control: true,
                    ..KeyModifiers::default()
                },
                action: "split_vertical".into(),
            }],
            left_alt_as_meta: false,
            right_alt_as_meta: true,
            enable_csi_u: true,
            enable_kitty_keyboard: true,
            mouse_reporting: false,
            scroll_on_output: false,
            scroll_on_keypress: true,
            answerback: "custom-answer".into(),
            ..TerminalSettingsV1::default()
        },
        ..TerminalProfile::default()
    };
    repository.save_terminal_profile(profile.clone()).unwrap();
    let settings = AppSettings {
        default_terminal_profile: profile.id,
        color_scheme: ColorScheme::GruvboxDark,
        key_bindings: profile.settings.key_bindings.clone(),
    };
    repository.save_settings(settings.clone()).unwrap();

    assert!(
        repository
            .load_terminal_profiles()
            .unwrap()
            .contains(&profile)
    );
    assert_eq!(repository.load_settings().unwrap(), settings);
    repository.shutdown().unwrap();
}

#[test]
fn reopening_preserves_explicit_old_default_font_instead_of_reseeding_it() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("font-profile.sqlite3");
    let repository = SqliteRepository::open(&path).unwrap();
    repository.migrate().unwrap();
    let saved = TerminalProfile {
        settings: TerminalSettingsV1 {
            font_family: "Cascadia Mono".into(),
            font_size: 15.0,
            ..TerminalSettingsV1::default()
        },
        ..TerminalProfile::default()
    };
    repository.save_terminal_profile(saved.clone()).unwrap();
    repository.shutdown().unwrap();

    let reopened = SqliteRepository::open(&path).unwrap();
    reopened.migrate().unwrap();
    assert_eq!(reopened.load_terminal_profiles().unwrap(), vec![saved]);
    reopened.shutdown().unwrap();
}

#[test]
fn file_database_uses_required_pragmas_and_private_permissions() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("nested").join("catalog.sqlite3");
    let repository = SqliteRepository::open(&path).unwrap();
    repository.migrate().unwrap();

    let status = repository.database_status().unwrap();
    assert!(status.foreign_keys);
    assert_eq!(status.busy_timeout_ms, 5_000);
    assert_eq!(status.journal_mode, "wal");
    assert_eq!(status.private_file_is_secure, Some(true));
    repository.shutdown().unwrap();
}

#[test]
fn in_memory_database_reports_connection_pragmas() {
    let repository = SqliteRepository::open_in_memory().unwrap();
    let status = repository.database_status().unwrap();
    assert!(status.foreign_keys);
    assert_eq!(status.busy_timeout_ms, 5_000);
    assert_eq!(status.journal_mode, "memory");
    assert_eq!(status.private_file_is_secure, None);
    repository.shutdown().unwrap();
}

#[test]
fn shutdown_is_explicit_and_idempotent() {
    let repository = SqliteRepository::open_in_memory().unwrap();
    repository.shutdown().unwrap();
    repository.shutdown().unwrap();
    assert_eq!(repository.schema_versions(), Err(StorageError::QueueClosed));
}

#[cfg(feature = "test-support")]
#[test]
fn schema_contains_task_five_tables_indexes_and_checks() {
    use rshell_storage::TestCredentialValue;

    let repository = SqliteRepository::open_in_memory().unwrap();
    repository.migrate().unwrap();
    let schema = repository.test_schema().unwrap();
    let names = schema.keys().cloned().collect::<BTreeSet<_>>();
    for required in [
        "app_settings",
        "app_setting_values",
        "connection_groups",
        "connection_tags",
        "connections",
        "credential_operations",
        "idx_connection_groups_parent_position",
        "idx_connection_tags_tag",
        "idx_connections_group_position",
        "idx_connections_search",
        "schema_migrations",
        "terminal_profiles",
    ] {
        assert!(names.contains(required), "missing schema object {required}");
    }
    let operations = &schema["credential_operations"];
    assert!(operations.contains("put_new"));
    assert!(operations.contains("delete_old"));
    assert!(operations.contains("prepared"));
    assert!(operations.contains("vault_applied"));
    let connections = &schema["connections"];
    for column in [
        "id",
        "group_id",
        "name",
        "host",
        "port",
        "username",
        "transport",
        "authentication",
        "credential_ref",
        "identity_file",
        "host_key_policy",
        "remote_command",
        "note",
        "position",
        "terminal_profile_id",
        "terminal_overrides_json",
    ] {
        assert!(
            connections.contains(column),
            "missing connection column {column}"
        );
    }
    assert!(
        repository
            .test_credential_operation(TestCredentialValue::Valid, TestCredentialValue::Valid)
            .is_ok()
    );
    assert!(
        repository
            .test_credential_operation(TestCredentialValue::Invalid, TestCredentialValue::Valid)
            .is_err()
    );
    assert!(
        repository
            .test_credential_operation(TestCredentialValue::Valid, TestCredentialValue::Invalid)
            .is_err()
    );
    repository.shutdown().unwrap();
}

#[cfg(feature = "test-support")]
#[test]
fn worker_panic_and_disconnect_are_reported_as_crashed() {
    let repository = SqliteRepository::open_in_memory().unwrap();
    assert_eq!(repository.test_crash_worker(), Err(StorageError::Crashed));
    assert_eq!(repository.schema_versions(), Err(StorageError::Crashed));
}

#[test]
fn upgrades_old_schemas_without_replacing_settings_profiles_or_import_metadata() {
    use rshell_core::TerminalProfileId;
    use rshell_storage::{ConfigurationChange, ConfigurationCommitOutcome};

    fn protected_rows(connection: &rusqlite::Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
        let mut foreign_keys = connection.prepare("PRAGMA foreign_key_check").unwrap();
        assert!(foreign_keys.query([]).unwrap().next().unwrap().is_none());
        [
            "SELECT * FROM connection_groups ORDER BY id",
            "SELECT * FROM connections ORDER BY id",
            "SELECT * FROM connection_tags ORDER BY connection_id, tag",
            "SELECT * FROM credential_operations ORDER BY operation_id",
        ]
        .into_iter()
        .map(|sql| {
            let mut statement = connection.prepare(sql).unwrap();
            let rows = statement
                .query_map([], |row| {
                    (0..row.as_ref().column_count())
                        .map(|column| row.get(column))
                        .collect::<rusqlite::Result<Vec<rusqlite::types::Value>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert!(!rows.is_empty(), "保护断言必须覆盖非空数据：{sql}");
            rows
        })
        .collect()
    }

    for version in [1, 2] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("upgrade.sqlite3");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        connection
            .execute_batch(include_str!("../migrations/0001_initial.sql"))
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO schema_migrations VALUES(1, 'fixture');
                 UPDATE terminal_profiles SET name='旧配置';
                 UPDATE app_settings SET color_scheme='one_dark';",
            )
            .unwrap();
        // 引用仅为合成标识符，不创建或读取任何真实凭据库条目。
        connection
            .execute_batch(
                "INSERT INTO connection_groups VALUES(
                    '10000000-0000-4000-8000-000000000001', NULL, '测试目录', 0);
                 INSERT INTO connections(
                    id, group_id, name, host, port, username, transport, authentication,
                    credential_ref, identity_file, host_key_policy, remote_command, note,
                    position, terminal_profile_id, terminal_overrides_json)
                 VALUES(
                    '10000000-0000-4000-8000-000000000002',
                    '10000000-0000-4000-8000-000000000001',
                    '测试连接', 'host.example.invalid', 22, '', 'native_ssh', 'password',
                    'rshell://credential/10000000-0000-4000-8000-000000000003',
                    NULL, 'strict', NULL, '合成数据', 0,
                    '00000000-0000-0000-0000-000000000001', '{\"version\":1}');
                 INSERT INTO connection_tags VALUES(
                    '10000000-0000-4000-8000-000000000002', '迁移保留');
                 INSERT INTO credential_operations VALUES(
                    '10000000-0000-4000-8000-000000000004',
                    'rshell://credential/10000000-0000-4000-8000-000000000003',
                    'put_new', 'vault_applied', '2000-01-01T00:00:00Z');
                 INSERT INTO terminal_profiles(id, name, settings_json)
                 SELECT '10000000-0000-4000-8000-000000000005', '可删除配置', settings_json
                 FROM terminal_profiles WHERE id='00000000-0000-0000-0000-000000000001';",
            )
            .unwrap();
        if version == 2 {
            connection
                .execute_batch(include_str!("../migrations/0002_import_metadata.sql"))
                .unwrap();
            connection
                .execute_batch(
                    "INSERT INTO schema_migrations VALUES(2, 'fixture');
                     INSERT INTO app_setting_values VALUES('fixture-import', 'legacy-json');",
                )
                .unwrap();
        }
        let protected_before = protected_rows(&connection);

        let repository = SqliteRepository::open(&path).unwrap();
        repository.migrate().unwrap();
        repository.migrate().unwrap();
        assert_eq!(repository.schema_versions().unwrap(), vec![1, 2, 3]);
        let snapshot = repository.load_configuration().unwrap();
        assert_eq!(snapshot.revision, 0);
        assert_eq!(snapshot.settings.color_scheme, ColorScheme::OneDark);
        assert_eq!(snapshot.profiles[0].name, "旧配置");
        assert_eq!(snapshot.profiles[0].settings, TerminalSettingsV1::default());
        assert_eq!(protected_rows(&connection), protected_before);
        let catalog_before = repository.load_catalog().unwrap();
        assert_eq!(catalog_before.connections.len(), 1);

        let mut updated_profile = snapshot.profiles[0].clone();
        updated_profile.name = "事务更新".into();
        let change = ConfigurationChange {
            expected_revision: snapshot.revision,
            settings: AppSettings {
                color_scheme: ColorScheme::Nord,
                ..snapshot.settings.clone()
            },
            upsert_profiles: vec![updated_profile.clone()],
            delete_profiles: vec![TerminalProfileId(
                uuid::Uuid::parse_str("10000000-0000-4000-8000-000000000005").unwrap(),
            )],
        };
        #[cfg(feature = "test-support")]
        {
            let before_tables = repository.test_visible_tables().unwrap();
            for statement in 1..=4 {
                repository.inject_statement_failure_once(statement).unwrap();
                assert_eq!(
                    repository.commit_configuration(change.clone()),
                    Err(StorageError::Constraint)
                );
                assert_eq!(repository.load_configuration().unwrap(), snapshot);
                assert_eq!(repository.test_visible_tables().unwrap(), before_tables);
                assert_eq!(protected_rows(&connection), protected_before);
            }
        }
        assert_eq!(
            repository.commit_configuration(change.clone()),
            Ok(ConfigurationCommitOutcome::Applied { revision: 1 })
        );
        let committed = repository.load_configuration().unwrap();
        assert_eq!(committed.revision, 1);
        assert_eq!(committed.settings, change.settings);
        assert_eq!(committed.profiles, vec![updated_profile]);
        assert_eq!(repository.load_catalog().unwrap(), catalog_before);
        assert_eq!(protected_rows(&connection), protected_before);
        repository.shutdown().unwrap();

        let reopened = SqliteRepository::open(&path).unwrap();
        reopened.migrate().unwrap();
        assert_eq!(reopened.load_configuration().unwrap(), committed);
        assert_eq!(protected_rows(&connection), protected_before);
        reopened.shutdown().unwrap();
        if version == 2 {
            let marker: String = connection
                .query_row(
                    "SELECT value FROM app_setting_values WHERE key='fixture-import'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(marker, "legacy-json");
        }
    }
}

#[test]
fn failed_revision_migration_rolls_back_earlier_migrations_in_the_same_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("migration-failure.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("../migrations/0001_initial.sql"))
        .unwrap();
    connection
        .execute_batch(
            "INSERT INTO schema_migrations VALUES(1, 'fixture');
             CREATE TABLE configuration_revision(sentinel TEXT);
             INSERT INTO configuration_revision VALUES('保留');",
        )
        .unwrap();
    drop(connection);

    let repository = SqliteRepository::open(&path).unwrap();
    assert!(repository.migrate().is_err());
    assert_eq!(repository.schema_versions().unwrap(), vec![1]);
    assert_eq!(repository.load_settings().unwrap(), AppSettings::default());
    repository.shutdown().unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let has_metadata: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='app_setting_values')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!has_metadata);
    let sentinel: String = connection
        .query_row("SELECT sentinel FROM configuration_revision", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(sentinel, "保留");
}

#[test]
fn revision_exhaustion_rolls_back_new_and_legacy_writes() {
    use rshell_storage::ConfigurationChange;

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("revision-limit.sqlite3");
    let repository = SqliteRepository::open(&path).unwrap();
    repository.migrate().unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute("UPDATE configuration_revision SET revision=?1", [i64::MAX])
        .unwrap();
    drop(connection);
    let before = repository.load_configuration().unwrap();
    let mut settings = before.settings.clone();
    settings.color_scheme = ColorScheme::Dracula;
    let mut profile = before.profiles[0].clone();
    profile.name = "不能提交".into();
    assert_eq!(
        repository.commit_configuration(ConfigurationChange {
            expected_revision: before.revision,
            settings: settings.clone(),
            upsert_profiles: vec![profile.clone()],
            delete_profiles: vec![],
        }),
        Err(StorageError::Constraint)
    );
    assert_eq!(repository.load_configuration().unwrap(), before);
    assert_eq!(
        repository.save_settings(settings),
        Err(StorageError::Constraint)
    );
    assert_eq!(
        repository.save_terminal_profile(profile),
        Err(StorageError::Constraint)
    );
    assert_eq!(repository.load_configuration().unwrap(), before);
    repository.shutdown().unwrap();
}
