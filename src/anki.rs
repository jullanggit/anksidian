use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Handles interaction with AnkiConnect.
// Could maybe use a bit more type-safety, stuff like action <-> params,
// and model <-> fields could be linked, but we dont really need it here and
// it would complicate the serialization

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Request<P: Serialize> {
    action: Action,
    version: u8,
    params: P,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
enum Action {
    AddNote,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct AddNote {
    note: Note,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Note {
    deck_name: String,
    model_name: String,
    fields: HashMap<String, String>,
    options: Options,
    tags: Vec<String>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Options {
    allow_duplicate: bool,
    duplicate_scope: DuplicateScope,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
enum DuplicateScope {
    Deck,
}

#[derive(Deserialize, Debug)]
struct Response<T> {
    result: Option<T>,
    error: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(transparent)]
pub struct NoteId(pub u64);

pub async fn add_cloze_note(text: String, tags: Vec<String>) -> Result<NoteId, String> {
    let request = Request {
        action: Action::AddNote,
        version: 6,
        params: AddNote {
            note: Note {
                deck_name: "Obsidian".to_string(),
                model_name: "Cloze".to_string(),
                fields: HashMap::from([
                    ("Text".to_string(), text),
                    ("Back Extra".to_string(), String::new()),
                ]),
                options: Options {
                    allow_duplicate: false,
                    duplicate_scope: DuplicateScope::Deck,
                },
                tags,
            },
        },
    };

    let client = reqwest::Client::new();
    let response = client
        .post("http://localhost:8765")
        .json(&request)
        .send()
        .await
        .unwrap();
    if response.status().is_success() {
        let response: Response<NoteId> = response.json().await.unwrap();
        match (response.result, response.error) {
            (Some(result), None) => Ok(result),
            (None, Some(error)) => Err(error),
            (Some(_), Some(_)) => unreachable!("Both error and result"),
            (None, None) => unreachable!("Neither error nor result"),
        }
    } else {
        Err(format!("Error: Status: {}", response.status()))
    }
}
