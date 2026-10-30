use std::sync::{Arc, Barrier};

use rshell_core::{
    AppSettings, CatalogMutation, CatalogOutcome, ColorScheme, ConnectionProfile, KeyBinding,
    KeyCode, KeyModifiers, TerminalProfile, TerminalProfileId, TerminalSettingsV1,
};
use rshell_storage::{
    ConfigurationChange, ConfigurationCommitOutcome, ConfigurationSnapshot, SqliteRepository,
    StorageError,
};

fn profile(name: &str) -> TerminalProfile {
    TerminalProfile {
        id: TerminalProfileId::new(),
        name: name.into(),
        settings: TerminalSettingsV1::default(),
    }
}

fn binding() -> KeyBinding {
    KeyBinding {
        code: KeyCode::F(6),
        modifiers: KeyModifiers {
            control: true,
            ..KeyModifiers::default()
        },
        action: "new_tab".into(),
    }
}

fn change(before: &ConfigurationSnapshot) -> ConfigurationChange {
    ConfigurationChange {
        expected_revision: before.revision,
        settings: before.settings.clone(),
        upsert_profiles: Vec::new(),
        delete_profiles: Vec::new(),
    }
}

fn applied(repository: &SqliteRepository, change: ConfigurationChange, revision: u64) {
    assert_eq!(
        repository.commit_configuration(change),
        Ok(ConfigurationCommitOutcome::Applied { revision })
    );
}

fn rejected(
    repository: &SqliteRepository,
    before: &ConfigurationSnapshot,
    change: ConfigurationChange,
    case: &str,
) {
    assert_eq!(
        repository.commit_configuration(change),
        Err(StorageError::Constraint),
        "{case}"
    );
    assert_eq!(repository.load_configuration().unwrap(), *before, "{case}");
}

#[test]
fn full_configuration_round_trip_updates_deletes_and_survives_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("configuration.sqlite3");
    let repository = SqliteRepository::open(&path).unwrap();
    repository.migrate().unwrap();
    let initial = repository.load_configuration().unwrap();
    assert_eq!(initial.revision, 0);
    assert_eq!(initial.settings, AppSettings::default());
    assert_eq!(initial.profiles, vec![TerminalProfile::p0_default()]);

    let mut rich = profile("Unicode 配置");
    rich.settings = TerminalSettingsV1 {
        terminal_type: "screen-256color".into(),
        initial_cols: 132,
        initial_rows: 44,
        scrollback_lines: 42_000,
        font_family: "Cascadia Mono".into(),
        font_size: 13.5,
        color_scheme: ColorScheme::TokyoNight,
        key_bindings: vec![binding()],
        left_alt_as_meta: false,
        right_alt_as_meta: true,
        enable_csi_u: true,
        enable_kitty_keyboard: true,
        mouse_reporting: false,
        scroll_on_output: false,
        scroll_on_keypress: true,
        answerback: "test-answer".into(),
        ..TerminalSettingsV1::default()
    };
    let spare = profile("Spare");
    let mut create = change(&initial);
    create.settings.default_terminal_profile = rich.id;
    create.settings.color_scheme = ColorScheme::GruvboxDark;
    create.settings.key_bindings = vec![binding()];
    create.upsert_profiles = vec![rich.clone(), spare.clone()];
    applied(&repository, create.clone(), 1);
    let created = repository.load_configuration().unwrap();
    assert_eq!(created.revision, 1);
    assert_eq!(created.settings, create.settings);
    assert_eq!(created.profiles.len(), 3);
    assert!(created.profiles.contains(&TerminalProfile::p0_default()));
    assert!(created.profiles.contains(&rich));
    assert!(created.profiles.contains(&spare));

    rich.name = "Updated 配置".into();
    rich.settings.font_size = 17.0;
    rich.settings.color_scheme = ColorScheme::Nord;
    rich.settings.scroll_on_output = true;
    let mut update = change(&created);
    update.settings.color_scheme = ColorScheme::OneDark;
    update.upsert_profiles.push(rich.clone());
    update.delete_profiles.push(spare.id);
    applied(&repository, update.clone(), 2);
    let updated = repository.load_configuration().unwrap();
    assert_eq!(updated.settings, update.settings);
    assert_eq!(updated.profiles.len(), 2);
    assert!(updated.profiles.contains(&rich));
    assert!(updated.profiles.contains(&TerminalProfile::p0_default()));
    repository.shutdown().unwrap();

    let repository = SqliteRepository::open(&path).unwrap();
    repository.migrate().unwrap();
    assert_eq!(repository.load_configuration().unwrap(), updated);
    assert_eq!(repository.load_settings().unwrap(), updated.settings);
    assert_eq!(
        repository.load_terminal_profiles().unwrap(),
        updated.profiles
    );

    let mut switch_default = change(&updated);
    switch_default.settings.default_terminal_profile = TerminalProfile::p0_default().id;
    applied(&repository, switch_default, 3);
    let mut delete = change(&repository.load_configuration().unwrap());
    delete.delete_profiles.push(rich.id);
    applied(&repository, delete, 4);
    let expected = repository.load_configuration().unwrap();
    assert_eq!(expected.profiles, vec![TerminalProfile::p0_default()]);
    assert_eq!(expected.revision, 4);
    repository.shutdown().unwrap();

    let reopened = SqliteRepository::open(&path).unwrap();
    reopened.migrate().unwrap();
    assert_eq!(reopened.load_configuration().unwrap(), expected);
    reopened.shutdown().unwrap();
}

#[test]
fn unknown_profile_extensions_survive_reopen_copy_rename_legacy_saves_and_stale_cas() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("extensions.sqlite3");
    let repository = SqliteRepository::open(&path).unwrap();
    repository.migrate().unwrap();
    let initial = repository.load_configuration().unwrap();
    assert_eq!(initial.revision, 0);
    assert_eq!(initial.profiles, vec![TerminalProfile::p0_default()]);
    let schema_versions = repository.schema_versions().unwrap();
    #[cfg(feature = "test-support")]
    let schema = repository.test_schema().unwrap();
    repository.shutdown().unwrap();

    let extensions = serde_json::json!({
        "example.app": {"version": 999, "future": [null, true, 42, "元数据", [], {}]},
        "unknown.scalar": "保留",
        "unknown.null": null
    });
    let mut settings_json = serde_json::to_value(&initial.profiles[0].settings).unwrap();
    settings_json["extensions"] = extensions.clone();
    let sqlite = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        sqlite
            .execute(
                "UPDATE terminal_profiles SET settings_json=?1 WHERE id=?2",
                rusqlite::params![
                    settings_json.to_string(),
                    initial.profiles[0].id.0.to_string()
                ],
            )
            .unwrap(),
        1
    );
    drop(sqlite);

    let repository = SqliteRepository::open(&path).unwrap();
    repository.migrate().unwrap();
    let before = repository.load_configuration().unwrap();
    assert_eq!(before.revision, 0);
    assert_eq!(before.settings, initial.settings);
    assert_eq!(
        serde_json::to_value(&before.profiles[0].settings).unwrap(),
        settings_json
    );
    let mut renamed = before.profiles[0].clone();
    renamed.name = "重命名配置".into();
    let mut copied = renamed.clone();
    copied.id = TerminalProfileId::new();
    copied.name = "复制配置".into();
    let mut proposal = change(&before);
    proposal.settings.default_terminal_profile = copied.id;
    proposal.upsert_profiles = vec![renamed.clone(), copied.clone()];
    applied(&repository, proposal, 1);
    let saved = repository.load_configuration().unwrap();
    assert_eq!(saved.profiles.len(), 2);
    assert!(saved.profiles.contains(&renamed));
    assert!(saved.profiles.contains(&copied));
    repository.shutdown().unwrap();

    let repository = SqliteRepository::open(&path).unwrap();
    repository.migrate().unwrap();
    let reopened = repository.load_configuration().unwrap();
    assert_eq!(reopened, saved);
    assert_eq!(repository.load_terminal_profiles().unwrap(), saved.profiles);
    assert_eq!(repository.load_settings().unwrap(), saved.settings);
    for profile in &reopened.profiles {
        assert_eq!(
            serde_json::to_value(&profile.settings).unwrap()["extensions"],
            extensions
        );
    }

    let mut legacy_copy = reopened
        .profiles
        .iter()
        .find(|profile| profile.id == copied.id)
        .unwrap()
        .clone();
    legacy_copy.name = "旧接口保存".into();
    repository
        .save_terminal_profile(legacy_copy.clone())
        .unwrap();
    let legacy_saved = repository.load_configuration().unwrap();
    assert_eq!(legacy_saved.revision, 2);
    assert!(legacy_saved.profiles.contains(&legacy_copy));
    legacy_copy.settings.extensions.insert(
        "example.app".into(),
        serde_json::json!({"version": 1000, "future": {"new": [false, null, "保留"]}}),
    );
    let mut update = change(&legacy_saved);
    update.upsert_profiles.push(legacy_copy.clone());
    applied(&repository, update, 3);
    let mut app_settings = saved.settings.clone();
    app_settings.color_scheme = ColorScheme::Nord;
    repository.save_settings(app_settings.clone()).unwrap();
    let latest = repository.load_configuration().unwrap();
    assert_eq!(latest.revision, 4);
    assert_eq!(latest.settings, app_settings);
    assert!(latest.profiles.contains(&legacy_copy));
    assert!(latest.profiles.contains(&renamed));

    let mut stale = change(&saved);
    copied.settings.extensions.clear();
    stale.upsert_profiles.push(copied);
    #[cfg(feature = "test-support")]
    let before_tables = repository.test_visible_tables().unwrap();
    assert_eq!(
        repository.commit_configuration(stale),
        Ok(ConfigurationCommitOutcome::Conflict { actual_revision: 4 })
    );
    assert_eq!(repository.load_configuration().unwrap(), latest);
    #[cfg(feature = "test-support")]
    {
        assert_eq!(repository.test_visible_tables().unwrap(), before_tables);
        assert_eq!(repository.test_schema().unwrap(), schema);
    }
    assert_eq!(repository.schema_versions().unwrap(), schema_versions);
    repository.shutdown().unwrap();

    let reopened = SqliteRepository::open(&path).unwrap();
    reopened.migrate().unwrap();
    assert_eq!(reopened.load_configuration().unwrap(), latest);
    assert_eq!(reopened.schema_versions().unwrap(), schema_versions);
    reopened.shutdown().unwrap();
}

#[test]
fn concurrent_commits_on_one_worker_have_one_winner_and_one_conflict() {
    let repository = Arc::new(SqliteRepository::open_in_memory().unwrap());
    repository.migrate().unwrap();
    let initial = repository.load_configuration().unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let workers = [ColorScheme::Nord, ColorScheme::Dracula].map(|color_scheme| {
        let repository = Arc::clone(&repository);
        let barrier = Arc::clone(&barrier);
        let mut proposal = change(&initial);
        proposal.settings.color_scheme = color_scheme;
        std::thread::spawn(move || {
            barrier.wait();
            (
                color_scheme,
                repository.commit_configuration(proposal).unwrap(),
            )
        })
    });
    barrier.wait();
    let outcomes = workers.map(|worker| worker.join().unwrap());
    let winner = outcomes
        .iter()
        .filter_map(|(scheme, outcome)| {
            (*outcome == ConfigurationCommitOutcome::Applied { revision: 1 }).then_some(*scheme)
        })
        .collect::<Vec<_>>();
    assert_eq!(winner.len(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|entry| {
                entry.1 == ConfigurationCommitOutcome::Conflict { actual_revision: 1 }
            })
            .count(),
        1
    );
    let final_state = repository.load_configuration().unwrap();
    assert_eq!(final_state.revision, 1);
    assert_eq!(final_state.settings.color_scheme, winner[0]);
    repository.shutdown().unwrap();
}

#[test]
fn separate_file_workers_serialize_stale_revision_without_losing_writes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("concurrent.sqlite3");
    let first = Arc::new(SqliteRepository::open(&path).unwrap());
    first.migrate().unwrap();
    let second = Arc::new(SqliteRepository::open(&path).unwrap());
    second.migrate().unwrap();
    let before = first.load_configuration().unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let workers = [(&first, "First"), (&second, "Second")].map(|(repository, name)| {
        let repository = Arc::clone(repository);
        let barrier = Arc::clone(&barrier);
        let added = profile(name);
        let mut proposal = change(&before);
        proposal.upsert_profiles.push(added.clone());
        std::thread::spawn(move || {
            barrier.wait();
            (added, repository.commit_configuration(proposal).unwrap())
        })
    });
    barrier.wait();
    let outcomes = workers.map(|worker| worker.join().unwrap());
    let winners = outcomes
        .iter()
        .filter(|entry| entry.1 == ConfigurationCommitOutcome::Applied { revision: 1 })
        .collect::<Vec<_>>();
    assert_eq!(winners.len(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|entry| {
                entry.1 == ConfigurationCommitOutcome::Conflict { actual_revision: 1 }
            })
            .count(),
        1
    );
    let final_state = first.load_configuration().unwrap();
    assert_eq!(second.load_configuration().unwrap(), final_state);
    assert_eq!(final_state.revision, 1);
    assert_eq!(final_state.profiles.len(), 2);
    assert!(final_state.profiles.contains(&winners[0].0));
    first.shutdown().unwrap();
    second.shutdown().unwrap();
}

#[test]
fn legacy_saves_advance_revision_and_invalidate_earlier_snapshot() {
    let repository = SqliteRepository::open_in_memory().unwrap();
    repository.migrate().unwrap();
    let before = repository.load_configuration().unwrap();
    let added = profile("Legacy");
    repository.save_terminal_profile(added.clone()).unwrap();
    let mut settings = before.settings.clone();
    settings.default_terminal_profile = added.id;
    repository.save_settings(settings.clone()).unwrap();
    let after = repository.load_configuration().unwrap();
    assert_eq!(after.revision, before.revision + 2);
    assert_eq!(after.settings, settings);
    assert!(after.profiles.contains(&added));
    assert_eq!(
        repository.commit_configuration(change(&before)),
        Ok(ConfigurationCommitOutcome::Conflict {
            actual_revision: after.revision,
        })
    );
    assert_eq!(repository.load_configuration().unwrap(), after);
    repository.shutdown().unwrap();
}

#[test]
fn previous_current_and_connection_referenced_defaults_cannot_be_deleted() {
    let repository = SqliteRepository::open_in_memory().unwrap();
    repository.migrate().unwrap();
    let initial = repository.load_configuration().unwrap();
    let next_default = profile("Next default");
    let referenced = profile("Referenced");
    let mut create = change(&initial);
    create.upsert_profiles = vec![next_default.clone(), referenced.clone()];
    applied(&repository, create, 1);
    let seeded = repository.load_configuration().unwrap();

    let mut switch_and_delete = change(&seeded);
    switch_and_delete.settings.default_terminal_profile = next_default.id;
    switch_and_delete.delete_profiles = vec![initial.settings.default_terminal_profile];
    rejected(
        &repository,
        &seeded,
        switch_and_delete,
        "切换默认时不得删除原默认",
    );

    let mut switch = change(&seeded);
    switch.settings.default_terminal_profile = next_default.id;
    applied(&repository, switch, 2);
    let switched = repository.load_configuration().unwrap();
    let mut delete_default = change(&switched);
    delete_default.delete_profiles.push(next_default.id);
    rejected(&repository, &switched, delete_default, "不得删除当前默认");

    let mut connection = ConnectionProfile::new("Test", "host.example.invalid");
    connection.terminal_profile_id = Some(referenced.id);
    assert_eq!(
        repository.apply(CatalogMutation::Create(connection.clone())),
        Ok(CatalogOutcome::Connection(connection.id))
    );
    let catalog_before = repository.load_catalog().unwrap();
    assert_eq!(catalog_before.connections[&connection.id].position, 0);
    let mut delete_referenced = change(&switched);
    delete_referenced.upsert_profiles.push(profile("不得残留"));
    delete_referenced.settings.color_scheme = ColorScheme::Nord;
    delete_referenced
        .delete_profiles
        .push(initial.settings.default_terminal_profile);
    delete_referenced.delete_profiles.push(referenced.id);
    rejected(
        &repository,
        &switched,
        delete_referenced,
        "不得删除连接引用的配置",
    );
    assert_eq!(repository.load_catalog().unwrap(), catalog_before);
    repository.shutdown().unwrap();
}

#[test]
fn invalid_changes_reject_atomically_and_stale_revision_wins_over_validation() {
    let repository = SqliteRepository::open_in_memory().unwrap();
    repository.migrate().unwrap();
    let initial = repository.load_configuration().unwrap();
    let existing = profile("Existing");
    let mut seed = change(&initial);
    seed.upsert_profiles.push(existing.clone());
    applied(&repository, seed, 1);
    let before = repository.load_configuration().unwrap();

    let mut duplicate_upsert = change(&before);
    duplicate_upsert.upsert_profiles = vec![existing.clone(), existing.clone()];
    let mut duplicate_delete = change(&before);
    duplicate_delete.delete_profiles = vec![existing.id, existing.id];
    let mut overlap = change(&before);
    overlap.upsert_profiles.push(existing.clone());
    overlap.delete_profiles.push(existing.id);
    let mut missing = change(&before);
    missing.upsert_profiles.push(profile("Must roll back"));
    missing.delete_profiles.push(TerminalProfileId::new());
    let mut unknown_default = change(&before);
    unknown_default
        .upsert_profiles
        .push(profile("Must roll back too"));
    unknown_default.settings.default_terminal_profile = TerminalProfileId::new();
    let mut invalid_binding = change(&before);
    invalid_binding.settings.key_bindings.push(KeyBinding {
        code: KeyCode::F(0),
        ..binding()
    });
    let mut deleted_default = change(&before);
    deleted_default.settings.default_terminal_profile = existing.id;
    deleted_default.delete_profiles.push(existing.id);
    for (case, invalid) in [
        ("重复保存 ID", duplicate_upsert),
        ("重复删除 ID", duplicate_delete),
        ("保存与删除 ID 重叠", overlap),
        ("删除不存在的 ID", missing),
        ("设置引用不存在的默认配置", unknown_default),
        ("设置按键绑定无效", invalid_binding),
        ("新默认配置不得在同一提交中删除", deleted_default),
    ] {
        rejected(&repository, &before, invalid, case);
    }
    for size in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut invalid = change(&before);
        let mut added = profile("Invalid font size");
        added.settings.font_size = size;
        invalid.upsert_profiles.push(added);
        rejected(&repository, &before, invalid, "非有限字号");
    }

    let mut stale_invalid = change(&initial);
    stale_invalid
        .delete_profiles
        .push(initial.settings.default_terminal_profile);
    stale_invalid.upsert_profiles.push(TerminalProfile {
        name: " ".into(),
        ..profile("Ignored")
    });
    assert_eq!(
        repository.commit_configuration(stale_invalid),
        Ok(ConfigurationCommitOutcome::Conflict {
            actual_revision: before.revision,
        })
    );
    assert_eq!(repository.load_configuration().unwrap(), before);
    repository.shutdown().unwrap();
}

#[test]
fn real_sqlite_error_after_profile_write_rolls_back_without_test_support() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("failure.sqlite3");
    let repository = SqliteRepository::open(&path).unwrap();
    repository.migrate().unwrap();
    let before = repository.load_configuration().unwrap();
    let mut proposal = change(&before);
    proposal.upsert_profiles.push(profile("Rollback"));
    proposal.settings.color_scheme = ColorScheme::Nord;

    let sqlite = rusqlite::Connection::open(&path).unwrap();
    sqlite
        .execute_batch(
            "CREATE TRIGGER fail_revision_update BEFORE UPDATE ON configuration_revision \
             BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
        )
        .unwrap();
    rejected(
        &repository,
        &before,
        proposal.clone(),
        "SQLite 版本写入失败",
    );
    sqlite
        .execute_batch("DROP TRIGGER fail_revision_update")
        .unwrap();
    applied(&repository, proposal, 1);
    repository.shutdown().unwrap();
}

#[cfg(feature = "test-support")]
#[test]
fn injected_failures_at_upsert_delete_settings_and_revision_roll_back() {
    let repository = SqliteRepository::open_in_memory().unwrap();
    repository.migrate().unwrap();
    let disposable = profile("Disposable");
    let mut seed = change(&repository.load_configuration().unwrap());
    seed.upsert_profiles.push(disposable.clone());
    applied(&repository, seed, 1);
    let before = repository.load_configuration().unwrap();
    let mut proposal = change(&before);
    proposal.upsert_profiles.push(profile("New"));
    proposal.delete_profiles.push(disposable.id);
    proposal.settings.color_scheme = ColorScheme::Nord;

    let before_tables = repository.test_visible_tables().unwrap();

    for (statement, case) in [
        (1, "保存配置"),
        (2, "删除配置"),
        (3, "写入设置"),
        (4, "递增版本"),
    ] {
        repository.inject_statement_failure_once(statement).unwrap();
        rejected(&repository, &before, proposal.clone(), case);
        assert_eq!(repository.test_visible_tables().unwrap(), before_tables);
    }
    applied(&repository, proposal, 2);
    repository.shutdown().unwrap();
}
