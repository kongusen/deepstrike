//! Single-elimination tournament — pairwise comparative judging.
//!
//! A pure control state machine: it holds the bracket and emits **abstract actions**; the SDK
//! runs a fresh-context judge agent per match and feeds back winners. No prompt assembly, no
//! I/O, no clock.
//!
//! Why a tournament instead of absolute scoring: comparative judgment ("which of these
//! two is better?") is more reliable than asking one agent to score 1000 items, and only
//! the current round's match-ups ever enter context — the deterministic loop holds the
//! whole bracket.
//!
//! ```text
//! entrants ──▶ JudgeRound{round 1, matches} ──▶ SDK runs N parallel pairwise judges
//!                    ▲                                      │
//!                    └────────── feed_round(winners) ◀──────┘
//!          (repeat until one survivor) ──▶ Done{winner}
//! ```

use crate::types::error::{DeepStrikeError, Result};

/// A participant. The SDK maps the id back to the real item being compared.
pub type EntrantId = String;

/// One pairwise match-up in a round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Index of this match within its round (0-based).
    pub id: u32,
    pub left: EntrantId,
    pub right: EntrantId,
}

/// What the SDK should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TournamentAction {
    /// Run one fresh-context judge per match this round (matches are independent — run
    /// them in parallel), then call [`Tournament::feed_round`] with the winners.
    JudgeRound { round: u32, matches: Vec<Match> },
    /// The bracket is resolved.
    Done { winner: EntrantId, rounds_used: u32 },
}

/// Single-elimination bracket control state machine.
#[derive(Debug)]
pub struct Tournament {
    /// Entrants advancing into the next round to be played.
    survivors: Vec<EntrantId>,
    /// Matches emitted for the current round, awaiting results.
    pending: Vec<Match>,
    /// Entrant that drew a bye this round (odd survivor count) — auto-advances.
    bye: Option<EntrantId>,
    /// Number of the most recently emitted round (0 before `start`).
    round: u32,
    done: bool,
}

impl Tournament {
    /// Build a tournament. Requires at least one entrant.
    pub fn new(entrants: Vec<EntrantId>) -> Result<Self> {
        if entrants.is_empty() {
            return Err(DeepStrikeError::InvalidConfig(
                "tournament requires at least one entrant".into(),
            ));
        }
        Ok(Self {
            survivors: entrants,
            pending: Vec::new(),
            bye: None,
            round: 0,
            done: false,
        })
    }

    /// Begin the tournament. A single entrant wins immediately (zero rounds);
    /// otherwise the first round of match-ups is emitted.
    pub fn start(&mut self) -> TournamentAction {
        self.emit_round_or_done()
    }

    /// Report the winners of the round last emitted by [`TournamentAction::JudgeRound`].
    /// `winners` must align one-to-one with that round's `matches`, and each winner must
    /// be one of the two entrants in its match.
    pub fn feed_round(&mut self, winners: Vec<EntrantId>) -> Result<TournamentAction> {
        if self.done {
            return Err(DeepStrikeError::InvalidConfig(
                "tournament already complete".into(),
            ));
        }
        if winners.len() != self.pending.len() {
            return Err(DeepStrikeError::InvalidConfig(format!(
                "expected {} winner(s) for round {}, got {}",
                self.pending.len(),
                self.round,
                winners.len()
            )));
        }
        for (w, m) in winners.iter().zip(&self.pending) {
            if w != &m.left && w != &m.right {
                return Err(DeepStrikeError::InvalidConfig(format!(
                    "winner '{w}' is not a participant in match {}",
                    m.id
                )));
            }
        }

        let mut next = winners;
        if let Some(bye) = self.bye.take() {
            next.push(bye);
        }
        self.survivors = next;
        self.pending.clear();
        Ok(self.emit_round_or_done())
    }

    /// Emit the next round of match-ups, or finish if only one survivor remains.
    fn emit_round_or_done(&mut self) -> TournamentAction {
        if self.survivors.len() == 1 {
            self.done = true;
            return TournamentAction::Done {
                winner: self.survivors[0].clone(),
                rounds_used: self.round,
            };
        }

        self.round += 1;
        let mut matches = Vec::with_capacity(self.survivors.len() / 2);
        let mut i = 0;
        while i + 1 < self.survivors.len() {
            matches.push(Match {
                id: (i / 2) as u32,
                left: self.survivors[i].clone(),
                right: self.survivors[i + 1].clone(),
            });
            i += 2;
        }
        // Odd count → the trailing entrant draws a bye and advances untouched.
        self.bye = if self.survivors.len() % 2 == 1 {
            self.survivors.last().cloned()
        } else {
            None
        };
        self.pending = matches.clone();
        TournamentAction::JudgeRound {
            round: self.round,
            matches,
        }
    }

    }

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(xs: &[&str]) -> Vec<EntrantId> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_entrants_is_error() {
        assert!(Tournament::new(vec![]).is_err());
    }

    #[test]
    fn single_entrant_wins_immediately() {
        let mut t = Tournament::new(ids(&["a"])).unwrap();
        match t.start() {
            TournamentAction::Done {
                winner,
                rounds_used,
            } => {
                assert_eq!(winner, "a");
                assert_eq!(rounds_used, 0);
            }
            _ => panic!("expected immediate Done"),
        }
    }

    #[test]
    fn two_entrants_one_round() {
        let mut t = Tournament::new(ids(&["a", "b"])).unwrap();
        match t.start() {
            TournamentAction::JudgeRound { round, matches } => {
                assert_eq!(round, 1);
                assert_eq!(matches.len(), 1);
                assert_eq!(
                    matches[0],
                    Match {
                        id: 0,
                        left: "a".into(),
                        right: "b".into()
                    }
                );
            }
            _ => panic!("expected JudgeRound"),
        }
        match t.feed_round(ids(&["b"])).unwrap() {
            TournamentAction::Done {
                winner,
                rounds_used,
            } => {
                assert_eq!(winner, "b");
                assert_eq!(rounds_used, 1);
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn four_entrants_two_rounds() {
        let mut t = Tournament::new(ids(&["a", "b", "c", "d"])).unwrap();
        let r1 = t.start();
        match r1 {
            TournamentAction::JudgeRound { round, matches } => {
                assert_eq!(round, 1);
                assert_eq!(matches.len(), 2);
            }
            _ => panic!(),
        }
        // a beats b, d beats c
        let r2 = t.feed_round(ids(&["a", "d"])).unwrap();
        match r2 {
            TournamentAction::JudgeRound { round, matches } => {
                assert_eq!(round, 2);
                assert_eq!(matches.len(), 1);
                assert_eq!(
                    matches[0],
                    Match {
                        id: 0,
                        left: "a".into(),
                        right: "d".into()
                    }
                );
            }
            _ => panic!(),
        }
        match t.feed_round(ids(&["d"])).unwrap() {
            TournamentAction::Done {
                winner,
                rounds_used,
            } => {
                assert_eq!(winner, "d");
                assert_eq!(rounds_used, 2);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn three_entrants_bye_advances() {
        let mut t = Tournament::new(ids(&["a", "b", "c"])).unwrap();
        match t.start() {
            TournamentAction::JudgeRound { round, matches } => {
                assert_eq!(round, 1);
                // only (a,b) plays; c gets a bye
                assert_eq!(matches.len(), 1);
                assert_eq!(
                    matches[0],
                    Match {
                        id: 0,
                        left: "a".into(),
                        right: "b".into()
                    }
                );
            }
            _ => panic!(),
        }
        // a beats b; survivors = [a, c (bye)]
        match t.feed_round(ids(&["a"])).unwrap() {
            TournamentAction::JudgeRound { round, matches } => {
                assert_eq!(round, 2);
                assert_eq!(
                    matches[0],
                    Match {
                        id: 0,
                        left: "a".into(),
                        right: "c".into()
                    }
                );
            }
            _ => panic!(),
        }
        match t.feed_round(ids(&["c"])).unwrap() {
            TournamentAction::Done {
                winner,
                rounds_used,
            } => {
                assert_eq!(winner, "c");
                assert_eq!(rounds_used, 2);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn eight_entrants_three_rounds() {
        let mut t = Tournament::new(ids(&["1", "2", "3", "4", "5", "6", "7", "8"])).unwrap();
        let mut action = t.start();
        let mut last_round = 0;
        loop {
            match action {
                TournamentAction::JudgeRound { round, matches } => {
                    last_round = round;
                    // winners = the left entrant of each match (deterministic)
                    let winners: Vec<EntrantId> = matches.iter().map(|m| m.left.clone()).collect();
                    action = t.feed_round(winners).unwrap();
                }
                TournamentAction::Done {
                    winner,
                    rounds_used,
                } => {
                    assert_eq!(winner, "1");
                    assert_eq!(rounds_used, 3);
                    assert_eq!(last_round, 3);
                    break;
                }
            }
        }
    }

    #[test]
    fn wrong_winner_count_is_error() {
        let mut t = Tournament::new(ids(&["a", "b", "c", "d"])).unwrap();
        t.start();
        // round 1 has 2 matches; feeding 1 winner is invalid
        assert!(t.feed_round(ids(&["a"])).is_err());
    }

    #[test]
    fn winner_not_in_match_is_error() {
        let mut t = Tournament::new(ids(&["a", "b"])).unwrap();
        t.start();
        assert!(t.feed_round(ids(&["zzz"])).is_err());
    }

    #[test]
    fn feed_after_done_is_error() {
        let mut t = Tournament::new(ids(&["a", "b"])).unwrap();
        t.start();
        t.feed_round(ids(&["a"])).unwrap();
        assert!(t.feed_round(ids(&["a"])).is_err());
    }
}
