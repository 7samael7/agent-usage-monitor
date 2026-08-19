//! The applications this machine has, and what each one can actually tell us.
//!
//! Most AI coding tools write a full transcript to disk and **no token counts
//! at all**. That is the common case rather than the exception, and it is worth
//! saying plainly: of the agents found on the machine this was developed
//! against, two report per-request usage, one reports a daily total, one
//! reports usage only once its telemetry export is switched on, and three
//! report nothing.
//!
//! Listing the silent ones is the point. An application that is missing from a
//! monitor is indistinguishable from one that was used and cost nothing, and
//! the second reading is the dangerous one.

use std::path::{Path, PathBuf};

use aum_contract::{AdapterDescriptor, AdapterState, CapabilityState};

/// An application that writes its sessions to disk but no measurement of them.
pub struct Unmeasured {
    pub id: &'static str,
    pub display_name: &'static str,
    /// Paths, relative to home, whose existence means the application is here.
    pub roots: &'static [&'static str],
    /// What was actually found in those files, stated as a measurement rather
    /// than an opinion. Each of these was counted on a real machine.
    pub finding: &'static str,
    /// What to do instead, where there is something.
    pub remedy: &'static str,
}

/// Applications known to write sessions without usage.
///
/// Adding one is a row here. Each `finding` is a count taken from real files,
/// because "does not report tokens" is a claim about someone else's software
/// and should be evidence rather than assertion.
pub const UNMEASURED: &[Unmeasured] = &[
    Unmeasured {
        id: "cursor",
        display_name: "Cursor",
        roots: &[".cursor/chats", ".cursor"],
        finding: "Cursor stores conversations as blobs in per-chat SQLite files. Across 775 of \
                  them the content was prompts and completions, with no token count of any kind. \
                  Its `ai-tracking` database counts lines of accepted code, not tokens.",
        remedy: "Cursor reports usage in its own dashboard, which is an account-level figure \
                 rather than anything on this machine.",
    },
    Unmeasured {
        id: "jetbrains_ai",
        display_name: "JetBrains AI Assistant",
        roots: &["Library/Application Support/JetBrains"],
        finding: "The IDE keeps its chat history in `aia-task-history/*.events`, base64-wrapped \
                  JSON. Across 1,184 decoded events there was no field carrying a token, a cost \
                  or a usage object.",
        remedy: "The IDE's own AI usage window reports quota consumption for the account.",
    },
    Unmeasured {
        id: "junie",
        display_name: "Junie",
        roots: &[".junie"],
        finding: "Junie keeps `history.jsonl` and rotating logs. Neither carries token counts, \
                  and on this machine the history file was empty.",
        remedy: "Junie draws on the same JetBrains AI quota as the assistant above.",
    },
    Unmeasured {
        id: "gemini_cli",
        display_name: "Gemini CLI",
        roots: &[".gemini"],
        finding: "Gemini CLI is installed. It writes chats under `~/.gemini/tmp/*/chats/`, but \
                  there was nothing there to read on this machine, so no parser has been written \
                  against a real file.",
        remedy: "An adapter can be added once there is a session to verify it against — a parser \
                 written from documentation alone is exactly how a confident wrong number gets \
                 shipped.",
    },
];

impl Unmeasured {
    /// The first of this application's paths that exists.
    #[must_use]
    pub fn detect(&self, home: &Path) -> Option<PathBuf> {
        self.roots.iter().map(|r| home.join(r)).find(|p| p.exists())
    }
}

/// Describe an application that is present and cannot be measured.
///
/// Every capability is `Unsupported` and every one carries the same evidence,
/// because the reason is the same for all of them: the file has no numbers in
/// it.
#[must_use]
pub fn describe_unmeasured(app: &Unmeasured, home: &Path) -> AdapterDescriptor {
    let found = app.detect(home);
    let mut capabilities = std::collections::BTreeMap::new();
    for name in crate::probe::CAPABILITIES {
        capabilities.insert(
            *name,
            CapabilityState::Unsupported {
                reason: app.finding.to_owned(),
            },
        );
    }

    let mut notes = Vec::new();
    if found.is_some() {
        notes.push(format!(
            "Detected. {} writes its sessions to disk, but nothing that counts tokens.",
            app.display_name
        ));
        notes.push(app.finding.to_owned());
        notes.push(app.remedy.to_owned());
        notes.push(
            "Its usage is therefore absent from every total here. That is a gap in what can be \
             measured, not a claim that it cost nothing."
                .to_owned(),
        );
    } else {
        notes.push(format!(
            "{} was not found on this machine.",
            app.display_name
        ));
    }

    crate::probe::descriptor(
        app.id,
        app.display_name,
        if found.is_some() {
            AdapterState::Detected
        } else {
            AdapterState::NotInstalled
        },
        found.map(|p| p.display().to_string()),
        capabilities,
        notes,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn an_application_that_is_absent_is_reported_as_not_installed() {
        let empty = tempfile::tempdir().unwrap();
        for app in UNMEASURED {
            let d = describe_unmeasured(app, empty.path());
            assert_eq!(d.state, AdapterState::NotInstalled, "{}", app.id);
        }
    }

    #[test]
    fn a_detected_application_says_what_was_looked_at_and_what_was_missing() {
        let home = tempfile::tempdir().unwrap();
        let app = &UNMEASURED[0];
        std::fs::create_dir_all(home.path().join(app.roots[0])).unwrap();

        let d = describe_unmeasured(app, home.path());
        assert_eq!(d.state, AdapterState::Detected);
        assert!(
            d.notes
                .join(" ")
                .contains("not a claim that it cost nothing"),
            "silence must not read as zero: {:?}",
            d.notes
        );
        assert!(
            !d.capabilities.is_empty()
                && d.capabilities
                    .iter()
                    .all(|(_, c)| matches!(c, CapabilityState::Unsupported { .. })),
            "every capability should be unsupported for {}",
            app.id
        );
    }

    #[test]
    fn every_entry_states_evidence_rather_than_an_opinion() {
        // "Does not report tokens" is a claim about someone else's software.
        // Each row has to say what was looked at and how much of it.
        for app in UNMEASURED {
            assert!(!app.roots.is_empty(), "{}", app.id);
            assert!(
                app.finding.len() > 80,
                "{}: the finding should describe what was found",
                app.id
            );
            assert!(!app.remedy.is_empty(), "{}", app.id);
        }
    }

    #[test]
    fn no_two_applications_share_an_id() {
        let mut ids: Vec<&str> = UNMEASURED.iter().map(|a| a.id).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(before, ids.len());
    }
}
