use super::*;
use paperik_document::{
    Document,
    editor::{Edit, Position},
};
struct TestRoot(PathBuf);
impl TestRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "paperik-recovery-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestRoot {
    fn drop(&mut self) {
        // No recursive deletion: test-created generations and lock files only.
        if let Ok(entries) = fs::read_dir(&self.0) {
            for entry in entries.flatten() {
                let path = entry.path();
                if ordinary(&path, true)
                    && path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(session_name)
                {
                    if let Ok(files) = fs::read_dir(&path) {
                        for file in files.flatten() {
                            let p = file.path();
                            if ordinary(&p, false)
                                && (sequence(&p).is_some()
                                    || p.file_name().is_some_and(|n| n == "lock"))
                            {
                                let _ = fs::remove_file(p);
                            }
                        }
                    }
                    let _ = fs::remove_dir(path);
                }
            }
        }
        let _ = fs::remove_dir(&self.0);
    }
}
fn editor() -> Editor {
    Editor::new(
        Document::from_bytes(include_bytes!("../../../../tests/fixtures/formatting.docx").to_vec())
            .unwrap(),
    )
}
fn apply(editor: &mut Editor, text: &str) {
    let change = editor.prepare(Edit::Insert(text.into())).unwrap().unwrap();
    editor.commit(change).unwrap();
}
fn wait(autosave: &mut Autosave) -> Event {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(event) = autosave.poll().into_iter().next() {
            return event;
        }
        assert!(std::time::Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn active_session_is_not_claimed_and_corrupt_latest_falls_back() {
    let root = TestRoot::new();
    let mut session = Session::create(&root.0).unwrap();
    let mut e = editor();
    apply(&mut e, "first");
    session.checkpoint(&e, "Пример.docx").unwrap();
    let first = e.document().paragraphs.clone();
    assert!(find(&root.0, &BTreeSet::new()).unwrap().is_none());
    apply(&mut e, "second");
    session.checkpoint(&e, "Пример.docx").unwrap();
    let latest = session.records().unwrap()[0].1.clone();
    let bytes = fs::read(&latest).unwrap();
    fs::write(latest, &bytes[..bytes.len() / 2]).unwrap();
    drop(session);
    let recovered = find(&root.0, &BTreeSet::new()).unwrap().unwrap();
    assert!(recovered.previous_generation);
    assert_eq!(recovered.editor.document().paragraphs, first);
    assert!(recovered.editor.dirty());
}
#[test]
fn append_keeps_two_completed_generations_and_never_reuses_partial_names() {
    let root = TestRoot::new();
    let mut session = Session::create(&root.0).unwrap();
    let mut e = editor();
    for value in ["one", "two", "three"] {
        apply(&mut e, value);
        session.checkpoint(&e, "Draft.docx").unwrap();
    }
    assert_eq!(session.records().unwrap().len(), 2);
    let path = session.path.clone();
    fs::write(path.join("generation-00000000000000000004.pkr"), b"partial").unwrap();
    drop(session);
    let recovered = find(&root.0, &BTreeSet::new()).unwrap().unwrap();
    assert!(recovered.previous_generation);
    let mut resumed = recovered.session;
    apply(&mut e, "four");
    resumed.checkpoint(&e, "Draft.docx").unwrap();
    assert_eq!(resumed.records().unwrap()[0].0, 5);
    assert_eq!(resumed.records().unwrap().len(), 2);
}
#[test]
fn broken_write_preserves_previous_snapshot_and_reports_error() {
    let root = TestRoot::new();
    let mut session = Session::create(&root.0).unwrap();
    let mut e = editor();
    apply(&mut e, "kept");
    session.checkpoint(&e, "Draft.docx").unwrap();
    let next = session
        .path
        .join(format!("generation-{:020}.pkr", session.next));
    fs::create_dir(&next).unwrap();
    apply(&mut e, "not-written");
    assert!(session.checkpoint(&e, "Draft.docx").is_err());
    fs::remove_dir(next).unwrap();
    drop(session);
    let recovered = find(&root.0, &BTreeSet::new()).unwrap().unwrap();
    assert!(
        recovered.editor.document().paragraphs[0]
            .text()
            .starts_with("kept")
    );
    assert!(
        !recovered.editor.document().paragraphs[0]
            .text()
            .contains("not-written")
    );
}
#[test]
fn clean_close_and_save_do_not_resurrect_old_drafts() {
    let root = TestRoot::new();
    let mut autosave = Autosave::new(&root.0).unwrap();
    let mut e = editor();
    apply(&mut e, "draft");
    assert!(autosave.queue(&e, "Draft.docx").unwrap());
    autosave.clean().unwrap();
    assert!(wait(&mut autosave).result.is_ok());
    drop(autosave);
    assert!(find(&root.0, &BTreeSet::new()).unwrap().is_none());
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
    let mut autosave = Autosave::new(&root.0).unwrap();
    assert!(autosave.queue(&e, "Draft.docx").unwrap());
    autosave.finish().unwrap();
    assert!(find(&root.0, &BTreeSet::new()).unwrap().is_none());
}
#[test]
fn original_file_is_not_required_and_undo_redo_survive_worker_restart() {
    let root = TestRoot::new();
    let mut autosave = Autosave::new(&root.0).unwrap();
    let mut e = editor();
    let source = e.document().package.original.clone();
    apply(&mut e, "Я🚀");
    let selection = e.selection();
    assert!(autosave.queue(&e, "Lost source.docx").unwrap());
    assert!(wait(&mut autosave).result.is_ok());
    drop(autosave);
    let recovered = find(&root.0, &BTreeSet::new()).unwrap().unwrap();
    let mut restored = recovered.editor;
    assert_eq!(restored.selection(), selection);
    assert_eq!(&*restored.document().package.original, &*source);
    let change = restored.prepare(Edit::Undo).unwrap().unwrap();
    restored.commit(change).unwrap();
    assert!(!restored.dirty());
    let change = restored.prepare(Edit::Redo).unwrap().unwrap();
    restored.commit(change).unwrap();
    assert!(restored.dirty());
    let mut resumed = Autosave::resume(recovered.session).unwrap();
    restored.set_position(
        Position {
            paragraph: 0,
            offset: 0,
        },
        false,
    );
    apply(&mut restored, "new");
    assert!(resumed.queue(&restored, "Lost source.docx").unwrap());
    assert!(wait(&mut resumed).result.is_ok());
    resumed.finish().unwrap();
}
#[test]
fn only_corrupt_generations_remain_on_disk_and_are_not_silently_deleted() {
    let root = TestRoot::new();
    let session = Session::create(&root.0).unwrap();
    let path = session.path.join("generation-00000000000000000001.pkr");
    fs::write(&path, b"bad").unwrap();
    drop(session);
    assert!(find(&root.0, &BTreeSet::new()).is_err());
    assert!(path.exists());
}
#[test]
fn session_metadata_cannot_redirect_restoration_to_an_external_file() {
    let root = TestRoot::new();
    let mut session = Session::create(&root.0).unwrap();
    let mut e = editor();
    apply(&mut e, "draft");
    assert!(session.checkpoint(&e, "../elsewhere.docx").is_err());
    assert!(session.records().unwrap().is_empty());
}
#[test]
fn worker_reports_checkpoint_failure_and_keeps_the_last_good_generation() {
    let root = TestRoot::new();
    let mut autosave = Autosave::new(&root.0).unwrap();
    let mut e = editor();
    apply(&mut e, "kept");
    autosave.queue(&e, "Draft.docx").unwrap();
    assert!(wait(&mut autosave).result.is_ok());
    let expected = e.document().paragraphs.clone();
    apply(&mut e, "newer");
    autosave.queue(&e, "../invalid").unwrap();
    assert!(wait(&mut autosave).result.is_err());
    assert!(e.document().paragraphs[0].text().contains("newer"));
    drop(autosave);
    let recovered = find(&root.0, &BTreeSet::new()).unwrap().unwrap();
    assert_eq!(recovered.editor.document().paragraphs, expected);
}
#[test]
fn process_exit_without_destructors_releases_lock_and_preserves_verified_draft() {
    let root = TestRoot::new();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "recovery::tests::crash_child",
            "--nocapture",
        ])
        .env("PAPERIK_TEST_CRASH_ROOT", &root.0)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(78),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let recovered = find(&root.0, &BTreeSet::new()).unwrap().unwrap();
    assert!(
        recovered.editor.document().paragraphs[0]
            .text()
            .starts_with("unsaved before exit")
    );
    assert!(recovered.previous_generation);
}
#[test]
#[ignore = "invoked only by the parent crash-recovery test"]
fn crash_child() {
    let Some(root) = std::env::var_os("PAPERIK_TEST_CRASH_ROOT") else {
        return;
    };
    let mut session = Session::create(Path::new(&root)).unwrap();
    let mut e = editor();
    apply(&mut e, "unsaved before exit");
    session.checkpoint(&e, "Crash test.docx").unwrap();
    // A second generation is interrupted. Exit skips every destructor and cleanup.
    let mut partial =
        File::create_new(session.path.join("generation-00000000000000000002.pkr")).unwrap();
    partial.write_all(b"PAPERIK-SESSION").unwrap();
    partial.sync_all().unwrap();
    std::process::exit(78);
}
