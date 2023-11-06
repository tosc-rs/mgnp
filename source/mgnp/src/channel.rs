use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{registry::Service, tricky_pipe::bidi::BiDi, Id};

pub struct ClientChannel<S: Service> {
    ids: Ids,
    chan: BiDi<S::ServerMsg, DataFrame<S::ClientMsg>>,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct Ids {
    local_id: Id,
    remote_id: Id,
}

#[derive(Serialize, Deserialize)]
pub struct DataFrame<T> {
    ids: Ids,
    data: T,
}

impl<S: Service> ClientChannel<S> {
    pub(crate) fn new(ids: Ids, chan: BiDi<S::ServerMsg, DataFrame<S::ClientMsg>>) -> Self {
        Self { ids, chan }
    }

    pub async fn send(&self, message: S::ClientMsg) -> Result<(), S::ClientMsg> {
        self.chan
            .tx()
            .send(DataFrame {
                ids: self.ids,
                data: message,
            })
            .await
            .map_err(|err| err.into_inner().data)
    }
}
