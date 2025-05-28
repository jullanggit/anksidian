use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashMap;

// Handles interaction with AnkiConnect.
// Could maybe use a bit more type-safety, stuff like action <-> params,
// and model <-> fields could be linked, but we dont really need it here and
// it would complicate the serialization

const DECK: &str = "Obsidian";

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Request<P: Serialize> {
    action: Action,
    version: u8,
    params: P,
}
impl<P: Serialize> Request<P> {
    async fn request<R: DeserializeOwned>(&self, client: &reqwest::Client) -> Result<R, String> {
        let response = client
            .post("http://localhost:8765")
            .json(&self)
            .send()
            .await
            .expect("AnkiConnect should be reachable");

        if response.status().is_success() {
            let response: Response<R> = response.json().await.unwrap();
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
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
enum Action {
    AddNote,
    CreateDeck,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct AddNote {
    note: Note,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct CreateDeck {
    deck: String,
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

pub async fn add_cloze_note(
    text: String,
    tags: Vec<String>,
    client: &reqwest::Client,
) -> Result<NoteId, String> {
    ensure_deck_exists(client).await?;

    let request = Request {
        action: Action::AddNote,
        version: 6,
        params: AddNote {
            note: Note {
                deck_name: DECK.to_string(),
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

    request.request(client).await
}

/// Ensures that the deck `DECK` exists
async fn ensure_deck_exists(client: &reqwest::Client) -> Result<(), String> {
    let request = Request {
        // create deck won't overwrite
        action: Action::CreateDeck,
        version: 6,
        params: CreateDeck {
            deck: DECK.to_string(),
        },
    };
    request.request(client).await.map(|_: u64| {})
}
