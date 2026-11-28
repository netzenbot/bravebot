//! A condition the session works towards.
//!
//! `/goal` holds one sentence a person typed and puts it to a judge when a turn ends. Where the
//! judge says the condition does not hold yet, the work goes back for another turn; where it says
//! anything else, the goal is over. The condition is settled the moment it is typed and never
//! changes afterwards, so nothing a turn reads, writes or says can decide when this session is
//! allowed to stop.
//!
//! A goal starts no work of its own. It has a condition and no prompt, so an armed goal sits there
//! until the person sends something, and what it then keeps going is their own request.
//!
//! Nothing here is written to disk. A goal lives as long as the session that set it.

use crate::outcome::Work;
use crate::repeat::Loops;
use crate::turn::GoalHeld;

/// The word that takes a goal off, rather than setting one called `clear`.
const CLEAR: &str = "clear";

/// The word that holds a goal still, rather than setting one called `pause`.
const PAUSE: &str = "pause";

/// The word that arms a held goal again, rather than setting one called `resume`.
const RESUME: &str = "resume";

/// How many times a goal may send the work back before it gives up.
///
/// Every one of these is a whole turn with the conversation re-sent, so this is what stands
/// between a condition nobody can satisfy and a session that spends until somebody notices. It is
/// generous enough for the multi-turn work a goal is for and small enough that the bill for
/// getting a condition wrong is ten turns rather than an afternoon.
pub const MAX_ROUNDS: usize = 10;

/// How many rounds in a row may make no tool call before the goal stops sending the work back.
///
/// A turn that answers the judge in prose without running anything is sent back again with the
/// whole conversation, and nothing about the next round makes it likelier to run something. Three
/// leaves room for one reply that was only a question or a summary.
pub const MAX_IDLE_ROUNDS: usize = 3;

/// What a verdict of not-yet comes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Send the work back for another turn.
    SendBack,
    /// The rounds are spent, which ends the goal.
    Spent,
    /// Consecutive rounds made no tool call. The goal stays set and waits for the next prompt.
    Stalled,
}

/// What the argument to `/goal` asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// The bare word: say what the goal is, or that there is none.
    Report,
    /// Take the goal off.
    Clear,
    /// Keep the goal and its condition, and stop judging turns against it.
    Pause,
    /// Judge turns against a paused goal again.
    Resume,
    /// Work towards this condition.
    Set(String),
}

/// Read the argument to `/goal`.
///
/// Three literal words are reserved and the rest of the language is a condition, which is as narrow
/// as the reservation can be made: `clear the build directory first` sets a goal, because the word
/// is not the whole argument.
pub fn parse(argument: &str) -> Asked {
    let argument = argument.trim();
    if argument.is_empty() {
        return Asked::Report;
    }
    match argument {
        CLEAR => return Asked::Clear,
        PAUSE => return Asked::Pause,
        RESUME => return Asked::Resume,
        _ => {}
    }
    Asked::Set(argument.to_string())
}

/// The goal a session is working towards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    /// The condition, exactly as it was typed.
    condition: String,
    /// How many times the work has been sent back.
    rounds: usize,
    /// What the judge said last time it was asked.
    last: Option<String>,
    /// How many rounds in a row ended with no tool call made.
    idle: usize,
    /// Whether the person has held the goal still. The condition and the count are kept either way.
    paused: bool,
    /// Whether the last verdict stopped sending the work back with the goal still set (GOAL-20),
    /// until the next verdict (OPEN-1).
    stalled: bool,
    /// When the goal was armed. Counted through a pause, since the goal is still set.
    started: std::time::Instant,
    /// The session's token total when the goal was armed, so what the goal has cost is the
    /// difference and not a second tally kept beside the session's.
    tokens_at_start: u64,
    /// What the turns since the goal was armed did, for the judge (GROUND-4).
    record: Work,
    /// Whether a turn was running when the goal was armed. Its work began before the goal, so it
    /// is left out of the record.
    skip_the_running_turn: bool,
}

impl Running {
    /// Arm a goal. Nothing is sent: a goal has a condition and no prompt.
    ///
    /// `tokens` is what the session has spent so far, which is where [`Running::spent`] counts from.
    /// `mid_turn` says a turn is running, which the record leaves out.
    pub fn begin(condition: String, tokens: u64, mid_turn: bool) -> Self {
        Self {
            condition,
            rounds: 0,
            last: None,
            idle: 0,
            paused: false,
            stalled: false,
            started: std::time::Instant::now(),
            tokens_at_start: tokens,
            record: Work::default(),
            skip_the_running_turn: mid_turn,
        }
    }

    /// Add what a turn did to the record, however it ended and whether or not the goal is paused,
    /// since the work is done either way. The turn that was running when the goal was armed is
    /// left out (GROUND-4).
    pub fn worked(&mut self, work: Work) {
        match std::mem::replace(&mut self.skip_the_running_turn, false) {
            true => {}
            false => self.record.then(work),
        }
    }

    /// What the turns since the goal was armed did.
    pub fn record(&self) -> Work {
        self.record
    }

    /// How long the goal has been set, as of `now`.
    pub fn elapsed(&self, now: std::time::Instant) -> std::time::Duration {
        now.saturating_duration_since(self.started)
    }

    /// What the session has spent since the goal was armed, given the session's total now.
    pub fn spent(&self, tokens: u64) -> u64 {
        tokens.saturating_sub(self.tokens_at_start)
    }

    /// The moment the goal was armed, for a test that needs a clock it can move.
    pub fn started(&self) -> std::time::Instant {
        self.started
    }

    /// Whether turns are being judged against this goal, or the person has held it still.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Whether the goal is still set after it stopped sending the work back (GOAL-20).
    pub fn is_stalled(&self) -> bool {
        self.stalled
    }

    /// Hold the goal still, and say whether it was running. The condition and the rounds spent are
    /// kept, so resuming picks up where this left off.
    pub fn pause(&mut self) -> bool {
        !std::mem::replace(&mut self.paused, true)
    }

    /// Arm a held goal again, and say whether it was paused.
    pub fn resume(&mut self) -> bool {
        std::mem::replace(&mut self.paused, false)
    }

    pub fn condition(&self) -> &str {
        &self.condition
    }

    pub fn rounds(&self) -> usize {
        self.rounds
    }

    /// What the judge said the last time it was asked, or `None` before it has been.
    pub fn last_reason(&self) -> Option<&str> {
        self.last.as_deref()
    }

    /// How many more times the work may be sent back.
    pub fn left(&self) -> usize {
        MAX_ROUNDS.saturating_sub(self.rounds)
    }

    /// Record a verdict of not-yet, and say what to do about it.
    ///
    /// `tool_calls` is how many calls the turn just judged made, a count the driver holds and not
    /// anything a reply said. The reason is kept either way, because a goal that has just run out
    /// of rounds is exactly when a person wants to read what the judge kept saying.
    pub fn not_met(&mut self, reason: String, tool_calls: usize) -> Next {
        self.last = Some(reason);
        self.stalled = false;
        if self.left() == 0 {
            return Next::Spent;
        }
        self.idle = if tool_calls == 0 { self.idle + 1 } else { 0 };
        if self.idle >= MAX_IDLE_ROUNDS {
            // Counted again from the next prompt: that turn is judged on its own.
            self.idle = 0;
            self.stalled = true;
            return Next::Stalled;
        }
        self.rounds += 1;
        Next::SendBack
    }
}

/// What a verdict came to, decided once for every front end that runs a goal.
///
/// Each front end says the outcome in its own words. Which outcome it is, and what happens to the
/// goal, is decided by [`settle`] alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// The goal was paused or taken off while the check was out, so the verdict is not acted on
    /// and not reported (GOAL-10, GOAL-18).
    Ignored,
    /// The condition does not hold yet: send `prompt`, the driver's own sentence, as the next turn.
    CarryOn { prompt: String, reason: String },
    /// The condition holds. The goal is over.
    Met { reason: String },
    /// Nothing further would make the condition hold. The goal is over.
    Impossible { reason: String },
    /// The judge's answer did not begin with a verdict word. The goal is over.
    Unreadable,
    /// The check met something untrusted, so its answer is not read. The goal is over.
    Quarantined,
    /// The check's request failed. The goal is over.
    Failed { problem: String },
    /// The rounds are spent. The goal is over (GOAL-9).
    Spent { rounds: usize, reason: String },
    /// Consecutive rounds made no tool call. The goal stays set (GOAL-20).
    Stalled { reason: String },
}

/// Act on what a check of the goal came back with (GOAL-6).
///
/// `judged` is `Err` for a check whose request failed, carrying what a person can be told about
/// it. `tool_calls` is how many tool calls the judged turn made, a count the driver holds.
///
/// The order is fixed. A paused goal ignores the verdict, and a failed check does not end it. A
/// failed check ends any other goal. A goal taken off ignores the verdict. Then the verdict
/// decides.
pub fn settle(
    goal: &mut Option<Running>,
    judged: Result<super::Verdict, String>,
    tool_calls: usize,
) -> Settled {
    use super::Verdict;
    if goal.as_ref().is_some_and(Running::is_paused) {
        return Settled::Ignored;
    }
    let verdict = match judged {
        Ok(verdict) => verdict,
        Err(problem) => {
            *goal = None;
            return Settled::Failed { problem };
        }
    };
    let Some(running) = goal.as_mut() else {
        return Settled::Ignored;
    };
    match verdict {
        Verdict::NotMet { reason } => match running.not_met(reason.clone(), tool_calls) {
            Next::SendBack => Settled::CarryOn {
                prompt: super::carry_on(running.condition(), &reason),
                reason,
            },
            Next::Spent => {
                let rounds = running.rounds();
                *goal = None;
                Settled::Spent { rounds, reason }
            }
            Next::Stalled => Settled::Stalled { reason },
        },
        Verdict::Met { reason } => {
            *goal = None;
            Settled::Met { reason }
        }
        Verdict::Impossible { reason } => {
            *goal = None;
            Settled::Impossible { reason }
        }
        Verdict::Unreadable => {
            *goal = None;
            Settled::Unreadable
        }
        Verdict::Quarantined => {
            *goal = None;
            Settled::Quarantined
        }
    }
}

/// What starting a goal did to the loops a session held and to a goal already set (GOAL-11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    /// Whether a goal was already set, paused or not, and the new one took its place.
    pub replaced: bool,
    /// The number of each loop the goal ended, oldest first. Empty where none was live.
    pub loops_ended: Vec<usize>,
}

/// Set `new` as the session's goal, ending every live loop (GOAL-11).
///
/// A goal and a loop are never both running, and whichever was asked for second stands. A goal
/// already set is replaced, paused or not. Each client says what ended in its own words: the
/// terminal writes a note, and the desktop bridge sends `loop.ended` for each number and
/// `goal.changed`.
///
/// Both arguments are the session's own state, and nothing a turn read is in either.
pub fn start<C, N>(goal: &mut Option<Running>, loops: &mut Loops<C, N>, new: Running) -> Started {
    let loops_ended = loops.iter().map(|running| running.number()).collect();
    loops.stop_all();
    let replaced = goal.replace(new).is_some();
    Started {
        replaced,
        loops_ended,
    }
}

/// Take the goal off because a person's `/loop` or `/after` is starting, and say whether there
/// was one (GOAL-11).
///
/// Paused or not: a paused goal is not running, but it is still the session's goal. Called only
/// where the loop does start, so a line that is refused leaves the goal as it was.
pub fn taken_off_by_a_loop(goal: &mut Option<Running>) -> bool {
    goal.take().is_some()
}

/// Whether a goal stands in the way of a loop a turn arranged by asking to look again (GOAL-11,
/// LOOP-14).
///
/// A goal set or paused refuses it, since the person is the one who may replace a goal with a
/// loop. Read when the turn ends and not when it began, so a goal set while the turn ran counts.
pub fn refuses_a_turns_loop(goal: Option<&Running>) -> bool {
    goal.is_some()
}

/// What the session holds of a goal, as [`crate::turn::LaterLook::for_line`] reads it.
pub fn held(goal: Option<&Running>) -> GoalHeld {
    match goal {
        None => GoalHeld::None,
        Some(running) if running.is_paused() => GoalHeld::Paused,
        Some(_) => GoalHeld::InForce,
    }
}
