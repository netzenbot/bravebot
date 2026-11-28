//! PLUGIN-7: a hook a plugin carries fires in a turn only after the person said yes, and where
//! nobody can be asked it does not fire.
//!
//! Each test installs a real plugin under a state directory of its own, runs a real turn against a
//! scripted model, and looks at a marker file the hook script appends to. A session is the
//! `AccountSession` a task is built in, which is where the answers of a session are kept.

#![cfg(unix)]

use bravebot_agent::Workspace;
use bravebot_agent::chatgpt::AccountSession;
use bravebot_agent::confirm::{AnswersHooks, Decision, Unattended};
use bravebot_agent::turn::Task;
use bravebot_core::event::RecordingSink;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[path = "../test-support/mock_model.rs"]
mod mock_model;
use mock_model::{Scratch, config_for, reply_with, serve_sequence};

#[allow(
    dead_code,
    reason = "`turns.rs` is shared by several test binaries, and this one calls only its `run`"
)]
#[path = "../test-support/turns.rs"]
mod turns;

/// A state directory holding the plugin `demo`, whose one hook is on `turn-started` and appends a
/// line to the returned marker file.
fn a_home_with_a_plugin(scratch: &Path) -> (PathBuf, PathBuf) {
    a_home_with_a_plugin_on(scratch, "turn-started")
}

/// [`a_home_with_a_plugin`], with the hook on `moment`.
fn a_home_with_a_plugin_on(scratch: &Path, moment: &str) -> (PathBuf, PathBuf) {
    let marker = scratch.join("marker");
    let source = scratch.join("plugin-source");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(
        source.join("plugin.toml"),
        "name = \"demo\"\nversion = \"1.0.0\"\ndescription = \"A demo\"\nhooks = \"hooks.json\"\n",
    )
    .expect("manifest");
    std::fs::write(
        source.join("hooks.json"),
        format!(r#"{{"hooks":[{{"on":"{moment}","run":["${{PLUGIN_ROOT}}/hook.sh"]}}]}}"#),
    )
    .expect("hooks");
    let script = source.join("hook.sh");
    write_script(
        &script,
        &format!("#!/bin/sh\necho ran >> '{}'\n", marker.display()),
    );
    let home = scratch.join("state");
    install_plugin(&source, &home);
    (home, marker)
}

fn ran(marker: &Path) -> usize {
    std::fs::read_to_string(marker).map_or(0, |text| text.lines().count())
}

/// Write `body` to `script` and make it executable, with no program being started meanwhile.
///
/// A test that writes a program races every other test in this binary. Between a sibling thread's
/// fork and its exec the child holds a copy of every descriptor this process had open, including
/// the one the script is being written through, and `execve` answers `ETXTBSY` for a file anyone
/// holds open for writing. A hook that fails to start that way is not tried again, so a test sees
/// a marker that never appears. Every start of a program takes the sandbox's spawn gate, so a
/// write made under it has no fork overlapping it.
fn write_script(script: &Path, body: &str) {
    bravebot_sandbox::process::creating_a_channel(|| {
        std::fs::write(script, body).expect("script");
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755))
            .expect("executable");
    });
}

/// Install the plugin at `source` under `home`, with no program being started meanwhile, for the
/// reason [`write_script`] gives: the install copies the script, and the copy is what a hook runs.
fn install_plugin(source: &Path, home: &Path) {
    bravebot_sandbox::process::creating_a_channel(|| {
        bravebot_agent::plugins::install(source, home).expect("install");
    });
}

/// One turn in `workspace`, in `session`, with `home` as the state directory.
fn a_turn<C: bravebot_agent::confirm::Confirmer + Send>(
    workspace: &Workspace,
    home: &Path,
    session: &AccountSession,
    confirmer: &mut C,
) {
    let (endpoint, _received) = serve_sequence(vec![reply_with("done")]);
    turns::run(
        &config_for(&endpoint),
        &bravebot_net::Egress::new(),
        workspace,
        &Task::new("say something")
            .with_home(Some(home.to_path_buf()))
            .in_account_session(session.clone()),
        confirmer,
        &mut RecordingSink::new(),
    )
    .expect("the turn runs");
}

/// T1: a plugin holding a hook writes no approval and fires nothing until the question is answered
/// yes. The turn asks at the moment the hook would fire, the hook runs after the yes and not
/// before, and one line is written.
///
/// Rejects: a turn that never reads a plugin's hooks, one that fires without asking, and one that
/// asks and then ignores the answer.
#[test]
fn a_turn_fires_a_plugin_hook_only_after_a_yes() {
    let scratch = Scratch::new("plugin-hook-turn-yes");
    let workspace = Workspace::new(&scratch.path).expect("workspace");
    let (home, marker) = a_home_with_a_plugin(&scratch.path);
    assert!(!home.join("hooks-approved").exists());

    let mut person = AnswersHooks::new(Decision::Approve);
    a_turn(&workspace, &home, &AccountSession::begin(), &mut person);

    assert_eq!(person.asked.len(), 1, "{:?}", person.asked);
    assert_eq!(person.asked[0].plugin, "demo");
    assert_eq!(person.asked[0].moment, "turn-started");
    assert_eq!(ran(&marker), 1);
    let recorded = std::fs::read_to_string(home.join("hooks-approved")).expect("recorded");
    assert_eq!(recorded.lines().count(), 1, "{recorded}");
}

/// T2: a no leaves the hook silent and writes nothing. Two turns of one session ask once, and a
/// later session asks again.
///
/// Rejects: a no that is written, a no asked at every turn of a session, and a no that outlives
/// the session.
#[test]
fn a_no_leaves_a_plugin_hook_silent_and_the_next_session_asks_again() {
    let scratch = Scratch::new("plugin-hook-turn-no");
    let workspace = Workspace::new(&scratch.path).expect("workspace");
    let (home, marker) = a_home_with_a_plugin(&scratch.path);

    let session = AccountSession::begin();
    let mut refuses = AnswersHooks::new(Decision::Reject);
    a_turn(&workspace, &home, &session, &mut refuses);
    a_turn(&workspace, &home, &session, &mut refuses);
    assert_eq!(
        refuses.asked.len(),
        1,
        "a no was asked again in the session"
    );
    assert_eq!(ran(&marker), 0);
    assert!(!home.join("hooks-approved").exists());

    let mut agrees = AnswersHooks::new(Decision::Approve);
    a_turn(&workspace, &home, &AccountSession::begin(), &mut agrees);
    assert_eq!(agrees.asked.len(), 1, "a later session was not asked");
    assert_eq!(ran(&marker), 1);
}

/// T6, S15: a run where nobody can be asked fires no plugin hook, even when a yes is recorded for
/// it, and puts no question. A one-shot run and a piped session are built on a confirmer that
/// answers nothing about hooks, and this is the same case at the turn.
///
/// Rejects: a turn that fires a recorded plugin hook where nobody is there.
#[test]
fn a_turn_nobody_can_ask_about_fires_no_plugin_hook() {
    let scratch = Scratch::new("plugin-hook-turn-nobody");
    let workspace = Workspace::new(&scratch.path).expect("workspace");
    let (home, marker) = a_home_with_a_plugin(&scratch.path);

    let mut person = AnswersHooks::new(Decision::Approve);
    a_turn(&workspace, &home, &AccountSession::begin(), &mut person);
    assert_eq!(ran(&marker), 1, "control: the person's yes fires it");

    a_turn(&workspace, &home, &AccountSession::begin(), &mut Unattended);
    assert_eq!(
        ran(&marker),
        1,
        "a recorded hook fired where nobody was asked"
    );
}

/// S21, T3 at the turn: a script edited after a yes stops the hook in the next turn of the same
/// session, puts no new question, and the outcome's notices name the plugin once, in the turn that
/// found the change and not in the turns after it.
///
/// Rejects: a turn that fires the edited script, one that asks about the changed tree, one that
/// says nothing to the person, and one that says it again at every turn.
#[test]
fn a_script_edited_after_a_yes_stops_the_hook_and_the_turn_names_the_plugin_once() {
    let scratch = Scratch::new("plugin-hook-turn-edited");
    let workspace = Workspace::new(&scratch.path).expect("workspace");
    let (home, marker) = a_home_with_a_plugin(&scratch.path);
    let session = AccountSession::begin();
    let mut person = AnswersHooks::new(Decision::Approve);
    a_turn(&workspace, &home, &session, &mut person);
    assert_eq!(ran(&marker), 1);

    let script = bravebot_agent::plugins::installed_dir(&home, "demo").join("hook.sh");
    write_script(
        &script,
        &format!("#!/bin/sh\necho edited >> '{}'\n", marker.display()),
    );
    let first = a_turn_outcome(&workspace, &home, &session, &mut person);
    let again = a_turn_outcome(&workspace, &home, &session, &mut person);

    assert_eq!(ran(&marker), 1, "the edited script ran");
    assert_eq!(person.asked.len(), 1, "a changed tree was asked about");
    let naming = |notices: &[String]| notices.iter().filter(|n| n.contains("demo")).count();
    assert_eq!(naming(&first), 1, "{first:?}");
    assert_eq!(naming(&again), 0, "{again:?}");
}

/// [`a_turn`], handing back the notices the outcome carries.
fn a_turn_outcome<C: bravebot_agent::confirm::Confirmer + Send>(
    workspace: &Workspace,
    home: &Path,
    session: &AccountSession,
    confirmer: &mut C,
) -> Vec<String> {
    let (endpoint, _received) = serve_sequence(vec![reply_with("done")]);
    turns::run(
        &config_for(&endpoint),
        &bravebot_net::Egress::new(),
        workspace,
        &Task::new("say something")
            .with_home(Some(home.to_path_buf()))
            .in_account_session(session.clone()),
        confirmer,
        &mut RecordingSink::new(),
    )
    .expect("the turn runs")
    .notices
}

/// One turn that makes a `list_files` call, so `tool-finished` fires once, which a turn reaches
/// through the confirmer it lent out and not the one it was handed.
fn a_turn_with_a_call<C: bravebot_agent::confirm::Confirmer + Send>(
    workspace: &Workspace,
    home: &Path,
    session: &AccountSession,
    confirmer: &mut C,
) {
    let (endpoint, _received) = serve_sequence(vec![
        mock_model::tool_request("list_files", r#"{"directory":"."}"#),
        reply_with("done"),
    ]);
    turns::run(
        &config_for(&endpoint),
        &bravebot_net::Egress::new(),
        workspace,
        &Task::new("look around")
            .with_home(Some(home.to_path_buf()))
            .in_account_session(session.clone()),
        confirmer,
        &mut RecordingSink::new(),
    )
    .expect("the turn runs");
}

/// S10: a hook on `tool-finished` is asked about when the first call finishes, through the
/// confirmer the turn lent to its own handle, and fires after the yes. Where nobody can be asked it
/// does not fire. The call is a read, which would otherwise run ahead of the hook, so the turn
/// must also not skip the hook by reading ahead.
///
/// Rejects: a lent confirmer that says nobody can be asked, a moment that fires before the
/// answer, and a read-ahead that runs past a hook the person has not answered.
#[test]
fn a_hook_on_tool_finished_asks_through_the_lent_confirmer_and_fires_after_a_yes() {
    let scratch = Scratch::new("plugin-hook-tool-finished");
    let workspace = Workspace::new(&scratch.path).expect("workspace");
    let (home, marker) = a_home_with_a_plugin_on(&scratch.path, "tool-finished");

    let mut nobody = Unattended;
    a_turn_with_a_call(&workspace, &home, &AccountSession::begin(), &mut nobody);
    assert_eq!(ran(&marker), 0, "fired where nobody could be asked");

    let mut person = AnswersHooks::new(Decision::Approve);
    a_turn_with_a_call(&workspace, &home, &AccountSession::begin(), &mut person);
    assert_eq!(person.asked.len(), 1, "{:?}", person.asked);
    assert_eq!(person.asked[0].moment, "tool-finished");
    assert_eq!(ran(&marker), 1);
}

/// A yes already in `hooks-approved` for each hook the plugin `demo` carries on `moment`, as the
/// person's earlier answer would have left it.
fn a_recorded_yes(home: &Path, moment: &str) {
    let tree = bravebot_agent::plugins::read_record(home, "demo")
        .expect("a record")
        .digest;
    let hooks = bravebot_agent::plugins::installed_hooks(home, "demo")
        .expect("a hooks file")
        .expect("readable");
    for hook in hooks
        .hooks
        .declared()
        .iter()
        .filter(|hook| hook.moment().as_str() == moment)
    {
        let digest = bravebot_config::hooks_approved::Digest::of("demo", moment, hook.run(), &tree);
        bravebot_config::hooks_approved::record(home, "demo", &digest).expect("recorded");
    }
}

/// The session moments of an interface that can ask, begun in `session`.
fn moments_that_can_ask(
    home: &Path,
    directory: &Path,
    session: &AccountSession,
) -> bravebot_agent::hooks::SessionMoments {
    let mut moments = bravebot_agent::hooks::SessionMoments::read(Some(home), directory);
    moments.keep_answers(session.clone());
    moments
}

/// PLUGIN-7, S10, S11, S15 at `session-started`: a plugin's hook on it is asked about in a watched
/// session and fires after the yes, and not before. An interface that kept no answers (a one-shot
/// run, the bridge) fires none even with the yes recorded, and one whose confirmer cannot ask fires
/// none either.
///
/// Rejects: a start that fires a plugin's hook without asking, one that asks and ignores the answer,
/// one that fires for an interface that never called `keep_answers`, and one that honours a
/// recorded yes where nobody can be asked.
#[test]
fn a_plugin_hook_on_session_started_fires_only_after_a_yes_in_a_watched_session() {
    let scratch = Scratch::new("plugin-hook-session-started-yes");
    let (home, marker) = a_home_with_a_plugin_on(&scratch.path, "session-started");

    // An interface that kept no answers: nothing is asked and nothing fires.
    let mut one_shot = bravebot_agent::hooks::SessionMoments::read(Some(&home), &scratch.path);
    assert!(one_shot.started().is_empty());
    assert_eq!(ran(&marker), 0, "fired where no answers were kept");

    // A watched interface whose confirmer cannot ask, with a yes recorded: still nothing.
    a_recorded_yes(&home, "session-started");
    let mut nobody = Unattended;
    let mut cannot = moments_that_can_ask(&home, &scratch.path, &AccountSession::begin());
    cannot.started_asking(&mut nobody);
    assert_eq!(ran(&marker), 0, "fired where nobody could be asked");
    std::fs::remove_file(home.join("hooks-approved")).expect("forget the yes");

    let mut person = AnswersHooks::new(Decision::Approve);
    let mut moments = moments_that_can_ask(&home, &scratch.path, &AccountSession::begin());
    assert_eq!(ran(&marker), 0, "fired before the question");
    assert!(moments.may_ask_at_start());
    assert!(moments.started_asking(&mut person).is_empty());

    assert_eq!(person.asked.len(), 1, "{:?}", person.asked);
    assert_eq!(person.asked[0].plugin, "demo");
    assert_eq!(person.asked[0].moment, "session-started");
    assert_eq!(ran(&marker), 1);
    let recorded = std::fs::read_to_string(home.join("hooks-approved")).expect("recorded");
    assert_eq!(recorded.lines().count(), 1, "{recorded}");

    // Once however often it is asked, as for the person's own hook.
    moments.started_asking(&mut person);
    assert_eq!(ran(&marker), 1);
    assert_eq!(person.asked.len(), 1);
}

/// PLUGIN-7, S14 at `session-started`: a no leaves the hook silent and writes nothing, and a later
/// session, which holds no answers of its own, asks again.
///
/// Rejects: a no that is written, a no that fires, and a no that is kept for the next session.
#[test]
fn a_no_at_session_started_is_asked_again_in_a_later_session() {
    let scratch = Scratch::new("plugin-hook-session-started-no");
    let (home, marker) = a_home_with_a_plugin_on(&scratch.path, "session-started");

    let mut refuses = AnswersHooks::new(Decision::Reject);
    moments_that_can_ask(&home, &scratch.path, &AccountSession::begin())
        .started_asking(&mut refuses);
    assert_eq!(refuses.asked.len(), 1);
    assert_eq!(ran(&marker), 0);
    assert!(!home.join("hooks-approved").exists());

    let mut agrees = AnswersHooks::new(Decision::Approve);
    moments_that_can_ask(&home, &scratch.path, &AccountSession::begin())
        .started_asking(&mut agrees);
    assert_eq!(agrees.asked.len(), 1, "a later session was not asked");
    assert_eq!(ran(&marker), 1);
}

/// PLUGIN-7, S10 at `session-ended`: the interface is being left, so a plugin's hook on it fires only
/// when it already has a yes, and nothing is ever asked or written. With no yes it does not fire.
///
/// Rejects: an end that asks, one that fires an unanswered hook, one that ignores a recorded yes, and
/// one that fires for an interface that kept no answers.
#[test]
fn a_plugin_hook_on_session_ended_fires_only_one_already_answered_and_never_asks() {
    let scratch = Scratch::new("plugin-hook-session-ended");
    let (home, marker) = a_home_with_a_plugin_on(&scratch.path, "session-ended");

    // No yes: the session begins and ends and the hook does not fire.
    let mut person = AnswersHooks::new(Decision::Approve);
    let mut moments = moments_that_can_ask(&home, &scratch.path, &AccountSession::begin())
        .ending_within(MUST_HAPPEN_WITHIN);
    moments.started_asking(&mut person);
    moments.ended();
    assert_eq!(ran(&marker), 0, "an unanswered hook fired at the end");
    assert!(person.asked.is_empty(), "the end asked a question");
    assert!(!home.join("hooks-approved").exists());

    // A yes recorded earlier: it fires, and still nothing is asked.
    // The default bound is five seconds, which a script that a loaded machine starts late can
    // outlast and be stopped before it writes, so the bound is the one a wait for something that
    // must happen gets. What the end says about a hook that did not start or was stopped is
    // checked first, so such a failure names its cause and not a marker that is absent.
    a_recorded_yes(&home, "session-ended");
    let mut moments = moments_that_can_ask(&home, &scratch.path, &AccountSession::begin())
        .ending_within(MUST_HAPPEN_WITHIN);
    moments.started_asking(&mut person);
    let said = moments.ended();
    assert!(
        said.is_empty(),
        "the hook did not run and end well: {said:?}"
    );
    assert_eq!(ran(&marker), 1);
    assert!(person.asked.is_empty(), "the end asked a question");

    // An interface that kept no answers fires none, whatever is recorded.
    let mut one_shot = bravebot_agent::hooks::SessionMoments::read(Some(&home), &scratch.path);
    one_shot.started();
    one_shot.ended();
    assert_eq!(
        ran(&marker),
        1,
        "fired for an interface that kept no answers"
    );
}

/// A watcher over `watched`, for the plugin `demo`, that looks every 50 milliseconds, and the file.
fn watching_for_the_plugin(
    home: &Path,
    directory: &Path,
    session: &AccountSession,
) -> (bravebot_agent::hooks::Watching, PathBuf) {
    let watched = directory.join("watched");
    std::fs::write(&watched, "a").expect("the file");
    let now = std::time::Instant::now();
    let entries = bravebot_agent::hooks::FileChanged::read(Some(home), directory, now)
        .with_plugins(Some(home), session.clone(), now)
        .looking_every(std::time::Duration::from_millis(50));
    let watching = bravebot_agent::hooks::Watching::begin(entries).expect("an entry is kept");
    (watching, watched)
}

/// How long a wait for something that must happen gets. Long enough for a machine that is running
/// many other builds, and left early the moment the thing happens, so a run that is not loaded
/// does not wait for it.
const MUST_HAPPEN_WITHIN: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a check that something does not happen watches for it. It ends in a pass, so it is
/// kept short, and a loaded machine can only make it pass for a shorter time than intended and
/// never fail.
const MUST_NOT_HAPPEN_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);

/// Put the watcher's questions to `confirmer` until `done` holds of it or `within` passes, and
/// answer whether `done` held.
fn served_until<C: bravebot_agent::confirm::Confirmer>(
    watching: &bravebot_agent::hooks::Watching,
    confirmer: &mut C,
    within: std::time::Duration,
    done: impl Fn(&C) -> bool,
) -> bool {
    let deadline = std::time::Instant::now() + within;
    loop {
        watching.serve(confirmer);
        if done(confirmer) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// [`served_until`] `marker` holds `wanted` lines, and when it does not within
/// [`MUST_HAPPEN_WITHIN`], a failure that says how many questions `person` was asked and what the
/// watcher said about the hook. A hook that could not be started, or was stopped, leaves a marker
/// that never appears, and the watcher's sentence is the only thing that tells it from a hook that
/// was never run, and the questions from one never asked about.
#[track_caller]
fn served_until_ran(
    watching: &bravebot_agent::hooks::Watching,
    person: &mut AnswersHooks,
    marker: &Path,
    wanted: usize,
    what: &str,
) {
    assert!(
        served_until(watching, person, MUST_HAPPEN_WITHIN, |_| {
            ran(marker) >= wanted
        }),
        "{what}: asked {:?}, the watcher said {:?}",
        person.asked,
        watching.said()
    );
}

/// A state directory holding the plugin `demo`, whose one hook is on `file-changed` for the file
/// `watched` and appends a line to the returned marker file.
fn a_home_with_file_changed_entry(scratch: &Path, _name: &str) -> (PathBuf, PathBuf) {
    let marker = scratch.join("marker");
    let source = scratch.join("plugin-source");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(
        source.join("plugin.toml"),
        "name = \"demo\"\nversion = \"1.0.0\"\ndescription = \"A demo\"\nhooks = \"hooks.json\"\n",
    )
    .expect("manifest");
    std::fs::write(
        source.join("hooks.json"),
        r#"{"hooks":[{"on":"file-changed","path":"watched","run":["${PLUGIN_ROOT}/hook.sh"]}]}"#,
    )
    .expect("hooks");
    let script = source.join("hook.sh");
    write_script(
        &script,
        &format!("#!/bin/sh\necho ran >> '{}'\n", marker.display()),
    );
    let home = scratch.join("state");
    install_plugin(&source, &home);
    (home, marker)
}

/// PLUGIN-7, S10, S11 at `file-changed`: the first change that would run a plugin's entry is asked
/// about, and the entry runs after the yes and not before. Nothing is asked until the file changes.
///
/// Rejects: an entry that runs with no question, one that is asked before any change, one that runs
/// after a no, and a question that is never put to the person.
#[test]
fn a_plugin_hook_on_file_changed_asks_then_fires() {
    let scratch = Scratch::new("plugin-hook-file-changed-yes");
    let (home, marker) = a_home_with_file_changed_entry(&scratch.path, "yes");
    let session = AccountSession::begin();
    let (watching, watched) = watching_for_the_plugin(&home, &scratch.path, &session);
    let mut person = AnswersHooks::new(Decision::Approve);

    assert!(!served_until(
        &watching,
        &mut person,
        MUST_NOT_HAPPEN_WITHIN,
        |_| ran(&marker) >= 1
    ));
    assert!(person.asked.is_empty(), "asked before any change");

    std::fs::write(&watched, "changed").expect("change it");
    served_until_ran(&watching, &mut person, &marker, 1, "never ran");
    assert_eq!(person.asked.len(), 1, "{:?}", person.asked);
    assert_eq!(person.asked[0].plugin, "demo");
    assert_eq!(person.asked[0].moment, "file-changed");
    let recorded = std::fs::read_to_string(home.join("hooks-approved")).expect("recorded");
    assert_eq!(recorded.lines().count(), 1, "{recorded}");
}

/// PLUGIN-7, S14 at `file-changed`: a no runs nothing, is asked once in the session however often the
/// file changes, writes nothing, and is asked again in a later session. Where the watching interface
/// kept no answers (`with_plugins` not called) a plugin's entry is not watched at all.
///
/// Rejects: a no that runs, a no asked at every change, a no that is written, and a plugin entry
/// watched by an interface that never said it could ask.
#[test]
fn a_no_at_file_changed_is_asked_once_and_again_in_a_later_session() {
    let scratch = Scratch::new("plugin-hook-file-changed-no");
    let (home, marker) = a_home_with_file_changed_entry(&scratch.path, "no");
    let session = AccountSession::begin();
    let (watching, watched) = watching_for_the_plugin(&home, &scratch.path, &session);
    let mut refuses = AnswersHooks::new(Decision::Reject);

    std::fs::write(&watched, "changed").expect("change it");
    assert!(
        served_until(&watching, &mut refuses, MUST_HAPPEN_WITHIN, |person| {
            !person.asked.is_empty()
        }),
        "the change was never asked about"
    );
    assert!(!served_until(
        &watching,
        &mut refuses,
        MUST_NOT_HAPPEN_WITHIN,
        |_| ran(&marker) >= 1
    ));
    assert_eq!(refuses.asked.len(), 1);
    std::fs::write(&watched, "changed again").expect("change it again");
    assert!(!served_until(
        &watching,
        &mut refuses,
        MUST_NOT_HAPPEN_WITHIN,
        |_| ran(&marker) >= 1
    ));
    assert_eq!(
        refuses.asked.len(),
        1,
        "a no was asked again in the session"
    );
    assert!(!home.join("hooks-approved").exists());
    drop(watching);

    let later = AccountSession::begin();
    let (watching, watched) = watching_for_the_plugin(&home, &scratch.path, &later);
    let mut agrees = AnswersHooks::new(Decision::Approve);
    std::fs::write(&watched, "changed in a later session").expect("change it");
    served_until_ran(
        &watching,
        &mut agrees,
        &marker,
        1,
        "a later session's change never ran",
    );
    assert_eq!(agrees.asked.len(), 1, "a later session was not asked");
    drop(watching);

    // No `with_plugins`: the person's own entries only, so a plugin's is not even looked at.
    let only_own = bravebot_agent::hooks::FileChanged::read(
        Some(&home),
        &scratch.path,
        std::time::Instant::now(),
    );
    assert!(only_own.is_empty());
}

/// PLUGIN-7, S21 at the session moments and `file-changed`: a plugin whose files changed after the
/// install fires none of them, even with a yes recorded, puts no question, and is named in a
/// sentence once.
///
/// Rejects: a moment that skips the re-hash, one that asks about a changed tree, and one that says
/// nothing to the person.
#[test]
fn a_changed_tree_stops_the_session_moments_and_file_changed() {
    let scratch = Scratch::new("plugin-hook-moments-changed");
    let source = scratch.path.join("plugin-source");
    std::fs::create_dir_all(&source).expect("source");
    let marker = scratch.path.join("marker");
    std::fs::write(
        source.join("plugin.toml"),
        "name = \"demo\"\nversion = \"1.0.0\"\ndescription = \"A demo\"\nhooks = \"hooks.json\"\n",
    )
    .expect("manifest");
    std::fs::write(
        source.join("hooks.json"),
        r#"{"hooks":[
            {"on":"session-started","run":["${PLUGIN_ROOT}/hook.sh"]},
            {"on":"session-ended","run":["${PLUGIN_ROOT}/hook.sh"]},
            {"on":"file-changed","path":"watched","run":["${PLUGIN_ROOT}/hook.sh"]}
        ]}"#,
    )
    .expect("hooks");
    let script = source.join("hook.sh");
    write_script(
        &script,
        &format!("#!/bin/sh\necho ran >> '{}'\n", marker.display()),
    );
    let home = scratch.path.join("state");
    install_plugin(&source, &home);
    for moment in ["session-started", "session-ended", "file-changed"] {
        a_recorded_yes(&home, moment);
    }

    let installed = bravebot_agent::plugins::installed_dir(&home, "demo").join("hook.sh");
    write_script(
        &installed,
        &format!("#!/bin/sh\necho edited >> '{}'\n", marker.display()),
    );

    let mut person = AnswersHooks::new(Decision::Approve);
    let session = AccountSession::begin();
    let mut moments = moments_that_can_ask(&home, &scratch.path, &session);
    let started = moments.started_asking(&mut person);
    let ended = moments.ended();
    let (watching, watched) = watching_for_the_plugin(&home, &scratch.path, &session);
    std::fs::write(&watched, "changed").expect("change it");
    let mut said = Vec::new();
    // Waits up to two seconds for the watcher to say anything. What must not happen is a sentence
    // naming the plugin again, so the bound ends a check that something does not happen and a
    // later sentence is not waited for.
    for _ in 0..40 {
        watching.serve(&mut person);
        said.extend(watching.said());
        if !said.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    assert_eq!(ran(&marker), 0, "a changed plugin fired");
    assert!(person.asked.is_empty(), "a changed tree was asked about");
    let naming = |sentences: &[String]| sentences.iter().filter(|s| s.contains("demo")).count();
    assert_eq!(naming(&started), 1, "{started:?}");
    assert_eq!(naming(&ended), 0, "named again at the end: {ended:?}");
    assert_eq!(naming(&said), 0, "named again by the watcher: {said:?}");
}

/// PLUGIN-7, HOOK-2 at `file-changed`: a plugin's entry whose path is absolute or has a `..`
/// component is not watched, so a plugin's text never makes the watcher look at a file outside the
/// directory the session opened in. A change to either file asks nothing and runs nothing, and an
/// entry with a relative path in the same plugin still asks and then runs.
///
/// Rejects: a plugin entry watched at an absolute path, one watched at a path that climbs out of
/// the directory with `..`, and a check that drops every entry of a plugin that has such an entry.
#[test]
fn a_plugin_entry_naming_a_file_outside_the_directory_is_not_watched() {
    let scratch = Scratch::new("plugin-hook-file-changed-outside");
    let checkout = scratch.path.join("checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    let outside = scratch.path.join("outside");
    let beside = scratch.path.join("beside");
    std::fs::write(&outside, "a").expect("outside file");
    std::fs::write(&beside, "a").expect("beside file");
    let marker = scratch.path.join("marker");
    let source = scratch.path.join("plugin-source");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(
        source.join("plugin.toml"),
        "name = \"demo\"\nversion = \"1.0.0\"\ndescription = \"A demo\"\nhooks = \"hooks.json\"\n",
    )
    .expect("manifest");
    std::fs::write(
        source.join("hooks.json"),
        format!(
            r#"{{"hooks":[
                {{"on":"file-changed","path":"{}","run":["${{PLUGIN_ROOT}}/hook.sh","absolute"]}},
                {{"on":"file-changed","path":"../beside","run":["${{PLUGIN_ROOT}}/hook.sh","parent"]}},
                {{"on":"file-changed","path":"watched","run":["${{PLUGIN_ROOT}}/hook.sh","relative"]}}
            ]}}"#,
            outside.display()
        ),
    )
    .expect("hooks");
    let script = source.join("hook.sh");
    write_script(
        &script,
        &format!("#!/bin/sh\necho \"$1\" >> '{}'\n", marker.display()),
    );
    let home = scratch.path.join("state");
    install_plugin(&source, &home);

    let session = AccountSession::begin();
    let (watching, watched) = watching_for_the_plugin(&home, &checkout, &session);
    let mut person = AnswersHooks::new(Decision::Approve);

    std::fs::write(&outside, "changed, and longer").expect("change the absolute file");
    std::fs::write(&beside, "changed, and longer").expect("change the parent file");
    assert!(!served_until(
        &watching,
        &mut person,
        MUST_NOT_HAPPEN_WITHIN,
        |_| ran(&marker) >= 1
    ));
    assert!(
        person.asked.is_empty(),
        "a file outside the directory was watched: {:?}",
        person.asked
    );

    std::fs::write(&watched, "changed").expect("change the relative file");
    served_until_ran(
        &watching,
        &mut person,
        &marker,
        1,
        "the relative entry never ran",
    );
    assert_eq!(person.asked.len(), 1, "{:?}", person.asked);
    assert_eq!(person.asked[0].plugin, "demo");
    assert_eq!(
        std::fs::read_to_string(&marker).expect("marker"),
        "relative\n"
    );
}

/// An asker for `/plugin update` that says yes to the update and has no host to ask about.
struct UpdateYes;

impl bravebot_agent::plugins::Asker for UpdateYes {
    fn anybody_there(&self) -> bool {
        true
    }

    fn confirm_fetch_host(&mut self, _request: &bravebot_agent::confirm::FetchRequest) -> bool {
        false
    }

    fn confirm_install(&mut self, _staged: &bravebot_agent::plugins::Staged) -> bool {
        false
    }

    fn confirm_update(
        &mut self,
        _staged: &bravebot_agent::plugins::Staged,
        _installed: &bravebot_agent::plugins::Installed,
        _before: &bravebot_agent::plugins::Before,
    ) -> bool {
        true
    }
}

/// PLUGIN-7, PLUGIN-9: after an accepted update every hook of the plugin is asked about
/// again, the one whose argv changed and the one whose hook entry did not change, because the
/// approval is a digest that includes the plugin's tree. Before the update the same turn asks
/// nothing, since both hooks have a recorded yes (the control).
///
/// Rejects: an approval digest that leaves the tree out, so the unchanged hook keeps running on
/// a yes given for other files, and an update that leaves the old yes for the changed hook.
#[test]
fn after_an_update_every_hook_of_the_plugin_is_asked_about_again() {
    let scratch = Scratch::new("plugin-hook-update-asks-again");
    let workspace = Workspace::new(&scratch.path).expect("workspace");
    let source = scratch.path.join("plugin-source");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(
        source.join("plugin.toml"),
        "name = \"demo\"\nversion = \"1.0.0\"\ndescription = \"A demo\"\nhooks = \"hooks.json\"\n",
    )
    .expect("manifest");
    let write_hooks = |first_argument: &str| {
        std::fs::write(
            source.join("hooks.json"),
            format!(
                r#"{{"hooks":[
                    {{"on":"turn-started","run":["${{PLUGIN_ROOT}}/hook.sh","{first_argument}"]}},
                    {{"on":"turn-finished","run":["${{PLUGIN_ROOT}}/hook.sh","unchanged"]}}
                ]}}"#
            ),
        )
        .expect("hooks");
    };
    write_hooks("before");
    let marker = scratch.path.join("marker");
    let script = source.join("hook.sh");
    write_script(
        &script,
        &format!("#!/bin/sh\necho ran >> '{}'\n", marker.display()),
    );
    let home = scratch.path.join("state");
    install_plugin(&source, &home);
    for moment in ["turn-started", "turn-finished"] {
        a_recorded_yes(&home, moment);
    }

    let mut control = AnswersHooks::new(Decision::Approve);
    a_turn(&workspace, &home, &AccountSession::begin(), &mut control);
    assert!(
        control.asked.is_empty(),
        "control: a recorded yes asks nothing: {:?}",
        control.asked
    );
    assert_eq!(ran(&marker), 2, "control: both recorded hooks fire");

    write_hooks("after");
    // Every line the person's yes left, kept so it can be put back after the update withdraws them.
    let old_answers = std::fs::read_to_string(home.join("hooks-approved")).expect("answers");
    assert_eq!(
        bravebot_agent::plugins::update_typed("demo", &home, &mut UpdateYes),
        Ok(bravebot_agent::plugins::Updated::Replaced)
    );
    assert!(
        !home.join("hooks-approved").exists()
            || std::fs::read_to_string(home.join("hooks-approved"))
                .expect("answers")
                .is_empty(),
        "the update left an answer for the plugin"
    );
    // Put the old lines back as if the withdrawal had not happened: they are digests of the old
    // tree, so a digest that includes the tree does not match them for any hook of the new tree.
    std::fs::write(home.join("hooks-approved"), &old_answers).expect("put back");
    let mut person = AnswersHooks::new(Decision::Approve);
    a_turn(&workspace, &home, &AccountSession::begin(), &mut person);
    let mut moments: Vec<&str> = person.asked.iter().map(|request| request.moment).collect();
    moments.sort_unstable();
    assert_eq!(
        moments,
        ["turn-finished", "turn-started"],
        "the changed hook and the unchanged one are both asked about"
    );
    assert!(
        std::fs::read_to_string(home.join("hooks-approved"))
            .expect("recorded")
            .lines()
            .all(|line| line.ends_with(" demo")),
        "only this plugin's lines"
    );
}
