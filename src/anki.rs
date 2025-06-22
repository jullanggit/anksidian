use log::{debug, warn};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::HashMap,
    fmt::{Debug, Write},
    time::Duration,
};
use tokio::time::sleep;

// Handles interaction with AnkiConnect.
// Could maybe use a bit more type-safety, stuff like action <-> params,
// and model <-> fields could be linked, but we dont really need it here and
// it would complicate the serialization

const MAX_BACKOFF: u8 = 5;

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Request<P: Serialize + Debug> {
    action: Action,
    version: u8,
    params: P,
}
impl<P: Serialize + Debug> Request<P> {
    async fn request<R: DeserializeOwned + Debug>(
        &self,
        client: &reqwest::Client,
    ) -> Result<R, String> {
        let request = client.post("http://localhost:8765").json(&self);
        let mut i = 0;
        let response = loop {
            let timeout = Duration::from_millis(100 * 2_u64.pow(i.into()));
            match request
                .try_clone()
                .expect("request should be cloneable")
                .send()
                .await
            {
                Ok(response) => break response,
                Err(e) if i < MAX_BACKOFF => {
                    warn!("AnkiConnect request failed (attempt {i}): {e}. Retrying in {timeout:?}");
                    sleep(timeout).await;
                }
                Err(e) => panic!("AnkiConnect request failed: {e}"),
            }
            i += 1;
        };

        let response = if response.status().is_success() {
            let response: Response<R> = response.json().await.unwrap();
            match (response.result, response.error) {
                (Some(result), None) => Ok(result),
                (None, Some(error)) => Err(error),
                (Some(_), Some(_)) => Err("Both error and result".to_string()),
                (None, None) => Err("Neither error nor result".to_string()),
            }
        } else {
            Err(format!("Error: Status: {}", response.status()))
        };
        debug!("Got response {response:?} from AnkiConnect for request {self:?}");
        response
    }
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
enum Action {
    AddNote,
    CreateDeck,
    UpdateNote,
    FindNotes,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Note<T> {
    note: T,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct CreateDeck {
    deck: String,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct AddNote {
    deck_name: String,
    model_name: String,
    fields: HashMap<String, String>,
    options: Options,
    tags: Vec<String>,
}
impl AddNote {
    fn to_query(&self) -> String {
        let mut out = format!("deck:\"{}\" note:\"{}\"", self.deck_name, self.model_name);
        for (field, value) in &self.fields {
            write!(
                out,
                " \"{field}:{}\"",
                value.replace('\\', "\\\\").replace(':', "\\:")
            )
            .unwrap();
        }
        // TODO: if it becoes an issue add back tag searching, first with then without
        out
    }
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct UpdateNote {
    id: NoteId,
    fields: HashMap<String, String>,
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

#[derive(Serialize, Debug)]
struct Query<Q: Serialize + Debug> {
    query: Q,
}

#[derive(Deserialize, Debug)]
struct Response<T> {
    result: Option<T>,
    error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
#[serde(transparent)]
/// Contains a Unix Timestamp (so 13 decimal digits for the years 2001-2286)
pub struct NoteId(pub u64);

pub async fn add_cloze_note(
    text: String,
    tags: Vec<String>,
    deck: String,
    client: &reqwest::Client,
) -> Result<NoteId, String> {
    ensure_deck_exists(client, deck.clone()).await?;

    let note = AddNote {
        deck_name: deck.clone(),
        model_name: "Cloze".to_string(),
        fields: HashMap::from([
            ("Text".to_string(), text.clone()),
            ("Back Extra".to_string(), String::new()),
        ]),
        options: Options {
            allow_duplicate: false,
            duplicate_scope: DuplicateScope::Deck,
        },
        tags: tags.clone(),
    };
    let request = Request {
        action: Action::AddNote,
        version: 6,
        params: Note { note: &note },
    };

    let result = request.request(client).await;

    // handle duplicate note
    match result {
        Err(e) if &e == "cannot create note because it is a duplicate" => {
            let query = note.to_query();
            let request = Request {
                action: Action::FindNotes,
                version: 6,
                params: Query { query },
            };

            return Ok(*request
                .request::<Vec<_>>(client)
                .await?
                .first()
                .expect("Note should exist, if it is a duplicate"));
        }
        other => other,
    }
}

pub async fn update_cloze_note(
    text: String,
    id: NoteId,
    tags: Vec<String>,
    client: &reqwest::Client,
) -> Result<(), String> {
    let note = UpdateNote {
        fields: HashMap::from([
            ("Text".to_string(), text),
            ("Back Extra".to_string(), String::new()),
        ]),
        id,
        tags,
    };
    let request = Request {
        action: Action::UpdateNote,
        version: 6,
        params: Note { note },
    };

    match request.request(client).await {
        // return null, null on success
        Err(string) if &string == "Neither error nor result" => Ok(()),
        other => other,
    }
}

/// Ensures that the deck `DECK` exists
async fn ensure_deck_exists(client: &reqwest::Client, deck: String) -> Result<(), String> {
    let request = Request {
        // create deck won't overwrite
        action: Action::CreateDeck,
        version: 6,
        params: CreateDeck { deck },
    };
    request.request(client).await.map(|_: u64| {})
}
