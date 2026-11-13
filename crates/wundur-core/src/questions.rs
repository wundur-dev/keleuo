//! Questions an agent puts to the person (Claude Code's AskUserQuestion tool): it asks for
//! permission to run the tool, and the answer is the permission with the person's choices
//! added to the tool's input (`answers`, by question text).

use serde_json::{Value, json};

/// The tool that asks.
pub const TOOL: &str = "AskUserQuestion";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub question: String,
    /// A short tag for it.
    pub header: String,
    /// Each choice: its label and what it means.
    pub options: Vec<(String, String)>,
    /// More than one may be chosen.
    pub multi: bool,
}

/// The questions in a request to run `tool` with `input`; `None` when it is not a question.
pub fn parse(tool: &str, input: &Value) -> Option<Vec<Question>> {
    if tool != TOOL {
        return None;
    }
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let questions: Vec<Question> = input
        .get("questions")?
        .as_array()?
        .iter()
        .map(|q| Question {
            question: s(q, "question"),
            header: s(q, "header"),
            options: q
                .get("options")
                .and_then(Value::as_array)
                .map(|os| os.iter().map(|o| (s(o, "label"), s(o, "description"))).collect())
                .unwrap_or_default(),
            multi: q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false),
        })
        .collect();
    (!questions.is_empty()).then_some(questions)
}

/// The tool's input with the person's answers: for each question, its text and the labels
/// chosen (several joined by ", ").
pub fn answered(input: &Value, answers: &[(String, Vec<String>)]) -> Value {
    let mut out = input.clone();
    let map: serde_json::Map<String, Value> = answers.iter().map(|(q, a)| (q.clone(), json!(a.join(", ")))).collect();
    if let Some(obj) = out.as_object_mut() {
        obj.insert("answers".into(), Value::Object(map));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_questions_and_adds_the_answers() {
        // As Claude Code sends it (seen from claude 2.1.299).
        let input = json!({"questions": [{"question": "Which colour do you prefer?", "header": "Colour",
            "options": [{"label": "Red", "description": "Choose red"}, {"label": "Blue", "description": "Choose blue"}],
            "multiSelect": false}]});
        let qs = parse(TOOL, &input).unwrap();
        assert_eq!(qs[0].question, "Which colour do you prefer?");
        assert_eq!(qs[0].options[1], ("Blue".to_string(), "Choose blue".to_string()));
        assert!(!qs[0].multi);
        assert!(parse("Bash", &input).is_none());
        let out = answered(&input, &[("Which colour do you prefer?".into(), vec!["Red".into(), "Blue".into()])]);
        assert_eq!(out["answers"]["Which colour do you prefer?"], "Red, Blue");
        assert_eq!(out["questions"], input["questions"]);
    }
}
