//! Optional Rhai evaluation over caller-supplied facts; no collection or I/O.
use rhai::{Dynamic, Engine, Map, OptimizationLevel, Scope, AST};
use status_model::{Fact, Facts, Health};

#[derive(Debug, thiserror::Error)]
pub enum EvaluationError {
    #[error("invalid rule: {0}")]
    Invalid(String),
    #[error("rule {index} failed: {message}")]
    Execution { index: usize, message: String },
}
pub struct Rule {
    expression: String,
    health: Health,
    message: String,
}
impl Rule {
    pub fn new(
        expression: impl Into<String>,
        health: Health,
        message: impl Into<String>,
    ) -> Result<Self, EvaluationError> {
        let expression = expression.into();
        let message = message.into();
        if expression.is_empty() || expression.len() > 4096 || message.len() > 4096 {
            return Err(EvaluationError::Invalid(
                "expression or explanation size".into(),
            ));
        }
        Ok(Self {
            expression,
            health,
            message,
        })
    }
}
#[derive(Clone, Debug)]
pub struct Decision {
    health: Health,
    message: String,
}
impl Decision {
    pub fn health(&self) -> Health {
        self.health
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}
/// Compile once on the owner's evaluation worker. Rhai never enters HTTP handlers.
pub struct RuleSet {
    engine: Engine,
    rules: Vec<(AST, Decision)>,
    fallback: Decision,
}
impl RuleSet {
    pub fn compile(rules: Vec<Rule>, fallback: Health) -> Result<Self, EvaluationError> {
        if rules.len() > 128 {
            return Err(EvaluationError::Invalid("too many rules".into()));
        }
        let mut engine = Engine::new();
        engine.set_optimization_level(OptimizationLevel::None);
        engine.set_fail_on_invalid_map_property(true);
        engine.set_max_operations(10_000);
        engine.set_max_call_levels(16);
        engine.set_max_expr_depths(32, 16);
        engine.set_max_string_size(16_384);
        engine.set_max_array_size(256);
        engine.set_max_map_size(256);
        for symbol in [
            "eval", "import", "export", "print", "debug", "while", "loop", "for",
        ] {
            engine.disable_symbol(symbol);
        }
        engine.on_print(|_| {});
        engine.on_debug(|_, _, _| {});
        let rules = rules
            .into_iter()
            .map(|rule| {
                let ast = engine
                    .compile_expression(&rule.expression)
                    .map_err(|e| EvaluationError::Invalid(e.to_string()))?;
                Ok((
                    ast,
                    Decision {
                        health: rule.health,
                        message: rule.message,
                    },
                ))
            })
            .collect::<Result<_, EvaluationError>>()?;
        Ok(Self {
            engine,
            rules,
            fallback: Decision {
                health: fallback,
                message: String::new(),
            },
        })
    }
    pub fn evaluate(&self, facts: &Facts) -> Result<Decision, EvaluationError> {
        let values: Map = facts
            .values()
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    Fact::Boolean(v) => Dynamic::from(*v),
                    Fact::Integer(v) => Dynamic::from(*v),
                    Fact::Number(v) => Dynamic::from(*v),
                    Fact::Text(v) => Dynamic::from(v.clone()),
                };
                (key.as_str().into(), value)
            })
            .collect();
        for (index, (ast, decision)) in self.rules.iter().enumerate() {
            // Each condition receives fresh immutable input; mutations cannot leak.
            let mut scope = Scope::new();
            scope.push_constant("facts", values.clone());
            let matched = self
                .engine
                .eval_ast_with_scope::<bool>(&mut scope, ast)
                .map_err(|e| EvaluationError::Execution {
                    index,
                    message: e.to_string(),
                })?;
            if matched {
                return Ok(decision.clone());
            }
        }
        Ok(self.fallback.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use status_model::Id;
    #[test]
    fn evaluates_supplied_facts_without_any_source() {
        let rules = RuleSet::compile(
            vec![Rule::new("facts.age > 60", Health::Warning, "Job is late").unwrap()],
            Health::Healthy,
        )
        .unwrap();
        let facts = Facts::new([(Id::new("age").unwrap(), Fact::Integer(90))]).unwrap();
        assert_eq!(rules.evaluate(&facts).unwrap().health(), Health::Warning);
        assert!(rules.evaluate(&Facts::default()).is_err());
    }
    #[test]
    fn rejects_dynamic_execution_and_non_boolean_results() {
        assert!(RuleSet::compile(
            vec![Rule::new("eval(\"true\")", Health::Failed, "").unwrap()],
            Health::Healthy
        )
        .is_err());
        let rules = RuleSet::compile(
            vec![Rule::new("42", Health::Failed, "").unwrap()],
            Health::Healthy,
        )
        .unwrap();
        assert!(rules.evaluate(&Facts::default()).is_err());
    }
}
