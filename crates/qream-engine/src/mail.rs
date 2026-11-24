//! Durable mail: transport acceptance, read, acknowledgement and answer are distinct.
use crate::{
    Error, Result,
    storage::{self, io},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Queued,
    Shown,
    Read,
    Acked,
    Answered,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub to: String,
    pub body: String,
    pub at: String,
    pub state: Delivery,
    pub question: bool,
    pub answer: Option<String>,
}
/// Implementations must atomically verify idle and show mail; they never type into a working turn.
pub trait Agents {
    fn show_mail_if_idle(&self, recipient: &str, message: &Message) -> Result<bool>;
}
#[derive(Clone)]
pub struct Mail {
    root: PathBuf,
}
fn address(s: &str) -> Result<()> {
    let parts: Vec<_> = s.split('/').collect();
    for part in &parts {
        storage::token(part)?;
    }
    if matches!(
        parts.as_slice(),
        ["harness", "chief"] | [_, "controller"] | [_, "worker", _]
    ) {
        Ok(())
    } else {
        Err(Error::Denied(
            "mail address must be campaign/worker/task, campaign/controller or harness/chief"
                .into(),
        ))
    }
}
impl Mail {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.join("state/mail"),
        }
    }
    fn path(&self, to: &str, id: &str) -> Result<PathBuf> {
        address(to)?;
        storage::token(id)?;
        Ok(self.root.join(to).join(format!("{id}.json")))
    }
    pub fn send(
        &self,
        id: &str,
        from: &str,
        to: &str,
        body: &str,
        question: bool,
    ) -> Result<Message> {
        address(from)?;
        let p = self.path(to, id)?;
        let _guard = storage::lock(&self.root.join(".mail.lock"))?;
        if p.exists() {
            return Err(Error::Denied(format!("message {id} already sent to {to}")));
        }
        let m = Message {
            id: id.into(),
            from: from.into(),
            to: to.into(),
            // Every body passes the house mask before it is kept or delivered.
            body: crate::mask::Mask::house().apply(body),
            at: storage::now(),
            state: Delivery::Queued,
            question,
            answer: None,
        };
        storage::write(&p, &m)?;
        Ok(m)
    }
    pub fn broadcast(
        &self,
        id: &str,
        from: &str,
        recipients: &[String],
        body: &str,
    ) -> Result<Vec<Message>> {
        // Validate every recipient before mutation. Each recipient's id is independently deduplicated.
        for to in recipients {
            self.path(to, id)?;
        }
        recipients
            .iter()
            .map(|to| self.send(id, from, to, body, false))
            .collect()
    }
    pub fn inbox(&self, to: &str) -> Result<Vec<Message>> {
        address(to)?;
        let dir = self.root.join(to);
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut paths = io(&dir, fs::read_dir(&dir))?
            .map(|e| io(&dir, e).map(|e| e.path()))
            .collect::<Result<Vec<_>>>()?;
        paths.sort();
        paths
            .into_iter()
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .map(|p| storage::read(&p))
            .collect()
    }
    pub fn get(&self, to: &str, id: &str) -> Result<Message> {
        storage::read(&self.path(to, id)?)
    }
    fn update(
        &self,
        to: &str,
        id: &str,
        f: impl FnOnce(&mut Message) -> Result<()>,
    ) -> Result<Message> {
        let p = self.path(to, id)?;
        let _guard = storage::lock(&self.root.join(".mail.lock"))?;
        let mut m: Message = storage::read(&p)?;
        f(&mut m)?;
        storage::write(&p, &m)?;
        Ok(m)
    }
    pub fn read(&self, to: &str, id: &str) -> Result<Message> {
        self.update(to, id, |m| {
            if matches!(m.state, Delivery::Queued | Delivery::Shown) {
                m.state = Delivery::Read;
            }
            Ok(())
        })
    }
    pub fn ack(&self, to: &str, id: &str) -> Result<Message> {
        self.update(to, id, |m| {
            if m.state != Delivery::Answered {
                m.state = Delivery::Acked;
            }
            Ok(())
        })
    }
    pub fn answer(&self, to: &str, id: &str, body: &str) -> Result<Message> {
        self.update(to, id, |m| {
            if !m.question || m.answer.is_some() {
                return Err(Error::Denied("not an unanswered question".into()));
            }
            m.answer = Some(crate::mask::Mask::house().apply(body));
            m.state = Delivery::Answered;
            Ok(())
        })
    }
    pub fn deliver(&self, to: &str, agents: &dyn Agents) -> Result<usize> {
        let mut n = 0;
        for msg in self.inbox(to)? {
            if msg.state == Delivery::Queued {
                self.update(to, &msg.id, |m| {
                    if m.state == Delivery::Queued && agents.show_mail_if_idle(to, m)? {
                        m.state = Delivery::Shown;
                        n += 1;
                    }
                    Ok(())
                })?;
            }
        }
        Ok(n)
    }
    pub fn ask(
        &self,
        id: &str,
        from: &str,
        to: &str,
        body: &str,
        timeout: Duration,
    ) -> Result<Message> {
        self.send(id, from, to, body, true)?;
        let end = Instant::now() + timeout;
        loop {
            let m = self.get(to, id)?;
            if m.answer.is_some() {
                return Ok(m);
            }
            if Instant::now() >= end {
                return Err(Error::Denied(format!(
                    "question {id} timed out; remains in inbox"
                )));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
