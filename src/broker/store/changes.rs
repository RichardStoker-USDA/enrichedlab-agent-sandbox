//! The change feed (migration 061): a row for each change a view shows, written in the changing
//! transaction, read by each person from a commit-ordered position under that person's own access rule, and a
//! notification per committed change on channel `broker_changes`.
use super::*;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
};
use tokio_postgres::AsyncMessage;

/// A position in the feed, `x<transaction>-<row>`: every row at or before it has been read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeCursor {
    pub txid: u64,
    pub id: i64,
}

impl ChangeCursor {
    /// Reads a position as the stream sends it; anything else is not one.
    pub fn parse(text: &str) -> Option<Self> {
        let (txid, id) = text.strip_prefix('x')?.split_once('-')?;
        let digits =
            |s: &str| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit());
        if !digits(txid) || !digits(id) {
            return None;
        }
        Some(Self {
            txid: txid.parse().ok()?,
            id: id.parse().ok()?,
        })
    }

    pub fn render(&self) -> String {
        format!("x{}-{}", self.txid, self.id)
    }
}

/// What one read of the feed found for one person.
#[derive(Debug, PartialEq, Eq)]
pub struct ChangeRead {
    pub position: ChangeCursor,
    /// What to read again: `projects`, `project:<id>`, `tasks`, `task:<id>`, `dataset:<id>`, `lab:<key>`, `inbox`,
    /// `catalogs`.
    pub keys: Vec<String>,
    /// The projects among the keys.
    pub projects: Vec<Uuid>,
    /// Something changed what this person may read or do: read everything again.
    pub access_changed: bool,
    /// The position is older than the rows kept, or too much changed to list: read everything again.
    pub behind: bool,
}

impl ChangeRead {
    fn from_value(value: &Value) -> Option<Self> {
        let strings = |v: &Value| -> Option<Vec<String>> {
            v.as_array()?
                .iter()
                .map(|s| s.as_str().map(str::to_owned))
                .collect()
        };
        Some(Self {
            position: ChangeCursor::parse(value["position"].as_str()?)?,
            keys: strings(&value["keys"])?,
            projects: strings(&value["projects"])?
                .iter()
                .map(|s| Uuid::parse_str(s).ok())
                .collect::<Option<Vec<_>>>()?,
            access_changed: value["access_changed"].as_bool()?,
            behind: value["behind"].as_bool()?,
        })
    }
}

/// The notifications of one listening connection; it closes when the connection ends.
pub struct ChangeListener {
    pub client: tokio_postgres::Client,
    pub notifications: mpsc::Receiver<String>,
}

impl Store {
    /// The API serves the change stream only with migration 061 and its grants.
    pub async fn ready_for_change_feed(&self) -> StoreResult<()> {
        let functions = [
            "enrichedlab_agent.broker_change_feed(text,uuid,xid8,bigint,integer)",
            "enrichedlab_agent.broker_projects_busy(text,uuid,uuid[])",
            "enrichedlab_agent.broker_prune_changes(text)",
            "enrichedlab_agent.broker_get_group_lab_data(text,uuid)",
        ];
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(61) AND bool_and(COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(f), 'EXECUTE'), false)) \
                 FROM unnest($1::text[]) AS f",
                &[&&functions[..]],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, Option<bool>>(0) == Some(true) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_schema_not_ready",
            })
        }
    }

    /// What changed for `actor` after `after`, at most `limit` rows at once; with no position, the newest position.
    pub async fn read_changes(
        &self,
        actor: Uuid,
        after: Option<&ChangeCursor>,
        limit: i32,
    ) -> StoreResult<ChangeRead> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let txid = after.map(|a| a.txid.to_string());
        let id = after.map(|a| a.id);
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_change_feed($1,$2,$3::text::xid8,$4,$5)",
                &[&self.tenant, &actor, &txid, &id, &limit],
            )
            .await
            .map_err(database_error)?;
        ChangeRead::from_value(&row.get::<_, Value>(0)).ok_or(StoreError {
            kind: StoreErrorKind::Unavailable,
            code: "change_feed_invalid",
        })
    }

    /// Whether work still runs in each project `actor` may read, keyed by project id (at most 64 projects).
    pub async fn projects_busy(&self, actor: Uuid, projects: &[Uuid]) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_projects_busy($1,$2,$3)",
                &[&self.tenant, &actor, &projects],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Removes rows older than a day; returns how many.
    pub async fn prune_changes(&self) -> StoreResult<i64> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_prune_changes($1)",
                &[&self.tenant],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// One connection outside the pool that listens on `broker_changes`. Its notifications carry the tenant.
    pub async fn listen_for_changes(&self) -> Result<ChangeListener> {
        let (sender, notifications) = mpsc::channel(64);
        let client = match &self.listen.tls {
            None => {
                let (client, connection) = self.listen.config.connect(NoTls).await?;
                tokio::spawn(forward_notifications(connection, sender));
                client
            }
            Some(tls) => {
                let (client, connection) = self.listen.config.connect(tls.clone()).await?;
                tokio::spawn(forward_notifications(connection, sender));
                client
            }
        };
        client.batch_execute("LISTEN broker_changes").await?;
        Ok(ChangeListener {
            client,
            notifications,
        })
    }
}

/// Drives a listening connection and passes each notification on. A full channel already holds a wake-up, so the
/// extra one is dropped.
async fn forward_notifications<S, T>(
    mut connection: tokio_postgres::Connection<S, T>,
    sender: mpsc::Sender<String>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
    while let Some(message) = messages.next().await {
        match message {
            Ok(AsyncMessage::Notification(note)) => {
                if let Err(mpsc::error::TrySendError::Closed(_)) =
                    sender.try_send(note.payload().to_owned())
                {
                    return;
                }
            }
            Ok(_) => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_position_reads_back_as_written_and_nothing_else_is_one() {
        let cursor = ChangeCursor::parse("x812-44").unwrap();
        assert_eq!(cursor, ChangeCursor { txid: 812, id: 44 });
        assert_eq!(cursor.render(), "x812-44");
        for bad in [
            "",
            "x",
            "812-44",
            "x812",
            "x-44",
            "x812-",
            "x8a2-44",
            "x812-4-4",
            "x+812-44",
            "x123456789012345678901-1",
            "y812-44",
            "x812-44 ",
        ] {
            assert_eq!(ChangeCursor::parse(bad), None, "{bad}");
        }
        assert_eq!(ChangeCursor::parse("x18446744073709551616-1"), None);
    }

    #[test]
    fn a_feed_answer_is_read_whole_or_not_at_all() {
        let project = Uuid::new_v4();
        let answer = json!({"position": "x9-3", "keys": ["inbox", format!("project:{project}")],
            "projects": [project.to_string()], "access_changed": false, "behind": false});
        assert_eq!(
            ChangeRead::from_value(&answer),
            Some(ChangeRead {
                position: ChangeCursor { txid: 9, id: 3 },
                keys: vec!["inbox".into(), format!("project:{project}")],
                projects: vec![project],
                access_changed: false,
                behind: false,
            })
        );
        for broken in [
            json!({"position": "9-3", "keys": [], "projects": [], "access_changed": false, "behind": false}),
            json!({"position": "x9-3", "keys": [1], "projects": [], "access_changed": false, "behind": false}),
            json!({"position": "x9-3", "keys": [], "projects": ["p"], "access_changed": false, "behind": false}),
            json!({"position": "x9-3", "keys": [], "projects": [], "behind": false}),
        ] {
            assert_eq!(ChangeRead::from_value(&broken), None, "{broken}");
        }
    }
}
