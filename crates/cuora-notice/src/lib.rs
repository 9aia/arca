//! Process-wide UX notices — the `cuora: …` stderr notes that
//! are not audit events (loose-permission warnings, keyring
//! fallback notes, exporter warnings).
//!
//! Same contract as the audit wire's mute: [`mute`] holds
//! notices instead of writing; [`unmute`] replays them in order
//! once the holding surface releases the terminal. Notices are
//! UX, not wire — they never fail an operation and never carry
//! secret material.

use std::sync::Mutex;

struct State {
    muted: bool,
    held: Vec<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    muted: false,
    held: Vec::new(),
});

fn lock() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Emit an `cuora:`-prefixed notice on stderr — or hold it while
/// muted, replayed by [`unmute`].
pub fn note(body: &str) {
    {
        let mut st = lock();
        if st.muted {
            st.held.push(body.to_string());
            return;
        }
    }
    eprintln!("cuora: {body}");
}

/// Hold notices — a terminal-owning surface calls this before it
/// takes over the tty so nothing writes mid-frame.
pub fn mute() {
    lock().muted = true;
}

/// Release the hold and replay everything buffered, in order.
/// Idempotent — safe to call when nothing was muted or held.
pub fn unmute() {
    let held = {
        let mut st = lock();
        st.muted = false;
        std::mem::take(&mut st.held)
    };
    for body in held {
        eprintln!("cuora: {body}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn muted_notes_replay_in_order_on_unmute() {
        // only test in this binary — the process-global state is
        // safe to exercise directly
        mute();
        note("first");
        note("second");
        unmute();
        // a post-unmute note writes live again
        note("third");
        // drain anything the live writes queued (none while
        // unmuted) and reset for other tests
        unmute();
    }
}
