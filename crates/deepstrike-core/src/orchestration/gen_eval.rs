/// Generator-Evaluator loop control.
/// The kernel manages iteration state; SDK layer calls LLM for generate/evaluate.

#[derive(Debug, Clone)]
pub struct GenEvalConfig {
    pub max_iterations: u32,
    pub quality_threshold: f64,
}

impl Default for GenEvalConfig {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            quality_threshold: 0.8,
        }
    }
}

/// Result of an evaluation step (provided by SDK layer).
#[derive(Debug, Clone)]
pub struct EvalResult {
    pub score: f64,
    pub feedback: Option<String>,
    pub pass: bool,
}

/// State machine for the gen-eval loop.
#[derive(Debug)]
pub struct GenEvalLoop {
    config: GenEvalConfig,
    iteration: u32,
    best_score: f64,
}

/// What the SDK should do next.
#[derive(Debug, PartialEq)]
pub enum GenEvalAction {
    Generate {
        iteration: u32,
        feedback: Option<String>,
    },
    Done {
        iterations_used: u32,
        final_score: f64,
    },
}

impl GenEvalLoop {
    pub fn new(config: GenEvalConfig) -> Self {
        Self {
            config,
            iteration: 0,
            best_score: 0.0,
        }
    }

    /// Start the loop — first action is always Generate.
    pub fn start(&mut self) -> GenEvalAction {
        self.iteration = 1;
        GenEvalAction::Generate {
            iteration: 1,
            feedback: None,
        }
    }

    /// Feed evaluation result, get next action.
    pub fn step(&mut self, eval: EvalResult) -> GenEvalAction {
        if eval.score > self.best_score {
            self.best_score = eval.score;
        }

        if eval.pass || eval.score >= self.config.quality_threshold {
            return GenEvalAction::Done {
                iterations_used: self.iteration,
                final_score: eval.score,
            };
        }

        if self.iteration >= self.config.max_iterations {
            return GenEvalAction::Done {
                iterations_used: self.iteration,
                final_score: self.best_score,
            };
        }

        self.iteration += 1;
        GenEvalAction::Generate {
            iteration: self.iteration,
            feedback: eval.feedback,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_on_high_score() {
        let mut ge = GenEvalLoop::new(GenEvalConfig::default());
        ge.start();
        let action = ge.step(EvalResult {
            score: 0.9,
            feedback: None,
            pass: false,
        });
        assert!(matches!(action, GenEvalAction::Done { .. }));
    }

    #[test]
    fn iterates_on_low_score() {
        let mut ge = GenEvalLoop::new(GenEvalConfig {
            max_iterations: 3,
            quality_threshold: 0.8,
        });
        ge.start();
        let action = ge.step(EvalResult {
            score: 0.3,
            feedback: Some("improve X".into()),
            pass: false,
        });
        assert!(matches!(
            action,
            GenEvalAction::Generate { iteration: 2, .. }
        ));
    }

    #[test]
    fn stops_at_max_iterations() {
        let mut ge = GenEvalLoop::new(GenEvalConfig {
            max_iterations: 2,
            quality_threshold: 0.9,
        });
        ge.start();
        ge.step(EvalResult {
            score: 0.3,
            feedback: None,
            pass: false,
        });
        let action = ge.step(EvalResult {
            score: 0.5,
            feedback: None,
            pass: false,
        });
        assert!(matches!(
            action,
            GenEvalAction::Done {
                iterations_used: 2,
                ..
            }
        ));
    }
}
