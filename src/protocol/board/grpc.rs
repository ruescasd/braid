// SPDX-FileCopyrightText: 2024 Sequent Tech <legal@sequentech.io>
//
// SPDX-License-Identifier: AGPL-3.0-only
use anyhow::Result;

use board_messages::grpc::GrpcB3Message;
use rusqlite::params;
use rusqlite::Connection;
use std::io::Write;
use std::path::PathBuf;
use tracing::{info, warn};

use b3::messages::message::Message;
use board_messages::grpc::client::B3Client;

use strand::serialization::StrandDeserialize;

const MAX_MESSAGE_SIZE: usize = 1024 * 1024 * 1024;
const GRPC_TIMEOUT: u64 = 5 * 60;
const RETRIEVE_ALL_PERIOD: u64 = 5 * 60;

impl super::Board for GrpcB3 {
    type Factory = GrpcB3BoardParams;

    async fn get_messages(&mut self, last_id: Option<i64>) -> Result<Vec<(Message, i64)>> {
        let messages = if self.store_root.is_some() {
            // When using a store, only the messages not previously received will be requested
            self.store_and_return_messages(last_id).await?
        } else {
            // If last_id is None, use -1 as last_id
            let messages = self.get_remote_messages(last_id.unwrap_or(-1)).await?;

            messages
                .iter()
                .map(|m| {
                    let message = Message::strand_deserialize(&m.message)?;
                    let id = m.id;
                    Ok((message, id))
                })
                .collect::<Result<Vec<(Message, i64)>>>()?
        };

        Ok(messages)
    }

    async fn insert_messages(&mut self, messages: Vec<Message>) -> Result<()> {
        if messages.len() > 0 {
            self.client
                .put_messages(&self.board_name, &messages)
                .await?;
        }

        Ok(())
    }
}

pub struct GrpcB3Index {
    client: B3Client,
}
impl GrpcB3Index {
    pub fn new(url: &str) -> GrpcB3Index {
        let client = B3Client::new(url, MAX_MESSAGE_SIZE, GRPC_TIMEOUT);

        GrpcB3Index { client }
    }

    pub async fn get_boards(&self) -> Result<Vec<String>> {
        let boards = self.client.get_boards().await?;
        let boards = boards.into_inner();

        Ok(boards.boards)
    }
}

pub struct GrpcB3 {
    client: B3Client,
    pub(crate) board_name: String,
    store_root: Option<PathBuf>,
    step_counter: u64,
}
impl GrpcB3 {
    pub fn new(url: &str, board_name: &str, store_root: Option<PathBuf>) -> GrpcB3 {
        let client = B3Client::new(url, MAX_MESSAGE_SIZE, GRPC_TIMEOUT);

        GrpcB3 {
            client,
            board_name: board_name.to_string(),
            store_root,
            step_counter: 0,
        }
    }

    fn get_store(&self) -> Result<Connection> {
        let db_path = self
            .store_root
            .as_ref()
            .expect("only called when store_root is some")
            .join(&self.board_name);
        let connection = Connection::open(&db_path)?;
        // The autogenerated id column is used to establish an order that cannot be manipulated by the external board. Once a retrieved message is
        // stored and assigned a local id, it is not possible for later messages to have an earlier id.
        // The external_id column is used to retrieve _new_ messages as defined by the external board (to optimize bandwidth).
        connection.execute("CREATE TABLE if not exists MESSAGES(id INTEGER PRIMARY KEY AUTOINCREMENT, external_id INT NOT NULL UNIQUE, message BLOB NOT NULL, blob_hash BLOB NOT NULL UNIQUE)", [])?;

        Ok(connection)
    }

    async fn store_and_return_messages(
        &mut self,
        last_id: Option<i64>,
    ) -> Result<Vec<(Message, i64)>> {
        let connection = self.get_store()?;

        let external_last_id =
            connection.query_row("SELECT max(external_id) FROM messages;", [], |row| {
                row.get(0)
            });

        if external_last_id.is_err() {
            warn!(
                "sql error retrieving external_last_id {:?}",
                external_last_id
            );
        }

        self.step_counter += 1;
        let reset = self.step_counter % RETRIEVE_ALL_PERIOD == 0;
        let external_last_id = if reset {
            -1
        } else {
            external_last_id.unwrap_or(-1)
        };
        // When querying for all messages we use -1 as default lower limit (this requests uses the > comparator in sql)

        let messages = self.get_remote_messages(external_last_id).await?;

        if messages.len() > 0 {
            info!(
                "Retrieved {} messages remotely (last_id = {})",
                messages.len(),
                external_last_id
            );
        } else {
            print!(".");
            let _ = std::io::stdout().flush();
        }

        // FIXME verify message signatures before inserting in local store
        let mut statement = if reset {
            connection.prepare(
                "INSERT OR IGNORE INTO MESSAGES(external_id, message, blob_hash) VALUES(?1, ?2, ?3)",
            )?
        } else {
            connection.prepare(
                "INSERT INTO MESSAGES(external_id, message, blob_hash) VALUES(?1, ?2, ?3)",
            )?
        };

        connection.execute("BEGIN TRANSACTION", [])?;
        for message in messages {
            let hash = strand::hash::hash(&message.message)?;
            statement.execute(params![message.id, message.message, hash])?;
        }
        connection.execute("END TRANSACTION", [])?;

        let mut stmt =
            connection.prepare("SELECT id,message FROM MESSAGES where id > ?1 order by id asc")?;
        // When querying for all messages in the store we use -1 as a lower limit
        let rows = stmt.query_map([last_id.unwrap_or(-1)], |row| {
            Ok(MessageRow {
                id: row.get(0)?,
                message: row.get(1)?,
            })
        })?;

        let messages: Result<Vec<(Message, i64)>> = rows
            .map(|mr| {
                let row = mr?;
                let id = row.id;
                let message = Message::strand_deserialize(&row.message)?;
                Ok((message, id))
            })
            .collect();

        messages
    }

    // Returns all messages whose id > last_id.
    async fn get_remote_messages(&mut self, last_id: i64) -> Result<Vec<GrpcB3Message>> {
        let messages = self.client.get_messages(&self.board_name, last_id).await?;

        let messages = messages.into_inner();

        Ok(messages.messages)
    }
}

pub struct GrpcB3BoardParams {
    url: String,
    board_name: String,
    store_root: Option<PathBuf>,
}
impl GrpcB3BoardParams {
    pub fn new(url: &str, board_name: &str, store_root: Option<PathBuf>) -> GrpcB3BoardParams {
        GrpcB3BoardParams {
            url: url.to_string(),
            board_name: board_name.to_string(),
            store_root,
        }
    }
}

impl super::BoardFactory<GrpcB3> for GrpcB3BoardParams {
    async fn get_board(&self) -> Result<GrpcB3> {
        Ok(GrpcB3::new(
            &self.url,
            &self.board_name,
            self.store_root.clone(),
        ))
    }
}

struct MessageRow {
    id: i64,
    message: Vec<u8>,
}

#[cfg(test)]
pub(crate) mod tests {

    use board_messages::grpc::B3Client;
    use board_messages::grpc::GetMessagesRequest;
    use serial_test::serial;

    #[tokio::test]
    #[ignore]
    #[serial]
    async fn test_grpc_client() {
        let mut client = B3Client::connect("http://[::1]:50051").await.unwrap();

        let request = tonic::Request::new(GetMessagesRequest {
            board: "default".to_string(),
            last_id: -1,
        });

        let response = client.get_messages(request).await.unwrap();

        println!("RESPONSE={:?}", response.into_inner().messages);
    }
}