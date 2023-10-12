use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};

use actix::prelude::*;

use crate::common::sled_utils::TableSequence;

#[derive(Clone, prost::Message, Serialize, Deserialize)]
pub struct TableDefinition {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(uint32, tag = "2")]
    pub sequence_step: u32, // 0: None
}

impl TableDefinition {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        prost::Message::encode(self, &mut v).unwrap();
        v
    }

    pub fn from_bytes(v: &[u8]) -> anyhow::Result<Self> {
        Ok(prost::Message::decode(v)?)
    }
}

pub(crate) const TABLE_DEFINITION_TREE_NAME: &str = "tables";

pub struct TableInfo {
    pub name: Arc<String>,
    pub table_db_name: String,
    pub seq: Option<TableSequence>,
}

impl TableInfo {
    pub fn new(name: Arc<String>, db: Arc<sled::Db>, sequence_step: u32) -> Self {
        let table_name = format!("t_{}", &name);
        let seq = if sequence_step == 0 {
            None
        } else {
            Some(TableSequence::new(
                db,
                format!("seq_{}", &name),
                sequence_step as u64,
            ))
        };
        Self {
            name,
            table_db_name: table_name,
            seq,
        }
    }
}

pub struct TableManage {
    pub db: Arc<sled::Db>,
    pub table_map: HashMap<Arc<String>, TableInfo>,
}

impl TableManage {
    pub fn new(db: Arc<sled::Db>) -> Self {
        let mut s = Self {
            db,
            table_map: Default::default(),
        };
        s.load_tables();
        s
    }

    /// load table info from db
    fn load_tables(&mut self) {
        let tables = self.db.open_tree(TABLE_DEFINITION_TREE_NAME).unwrap();
        let mut iter = tables.iter();
        while let Some(Ok((_, v))) = iter.next() {
            if let Ok(definition) = TableDefinition::from_bytes(v.as_ref()) {
                let name = Arc::new(definition.name.to_owned());
                self.table_map.insert(
                    name.clone(),
                    TableInfo::new(name, self.db.clone(), definition.sequence_step),
                );
            }
        }
    }

    fn init_table(&mut self, name: Arc<String>, sequence_step: u32) {
        let tables = self.db.open_tree(TABLE_DEFINITION_TREE_NAME).unwrap();
        let definition = TableDefinition {
            name: name.as_ref().to_owned(),
            sequence_step,
        };
        tables
            .insert(name.as_bytes(), definition.to_bytes())
            .unwrap();
    }

    pub fn drop_table(&mut self, name: &Arc<String>) {
        if let Some(mut table) = self.table_map.remove(name) {
            if let Some(seq) = table.seq.as_mut() {
                seq.set_table_last_id(0).ok();
            }
            self.db.drop_tree(&table.table_db_name).ok();
        }
    }

    pub fn next_id(&mut self, name: Arc<String>, seq_step: u32) -> anyhow::Result<u64> {
        if let Some(table_info) = self.table_map.get_mut(&name) {
            if let Some(seq) = table_info.seq.as_mut() {
                seq.next_id()
            } else {
                Err(anyhow::anyhow!("the table {} seq is none", &name))
            }
        } else {
            self.init_table(name.clone(), seq_step);
            let mut table_info = TableInfo::new(name.clone(), self.db.clone(), 0);
            let r = table_info.seq.as_mut().unwrap().next_id();
            self.table_map.insert(name, table_info);
            r
        }
    }

    pub fn set_last_seq_id(&mut self, name: Arc<String>, last_seq_id: u64) {
        if let Some(table_info) = self.table_map.get_mut(&name) {
            if let Some(seq) = table_info.seq.as_mut() {
                seq.set_table_last_id(last_seq_id).ok();
            }
        }
    }

    pub fn insert<K>(
        &mut self,
        name: Arc<String>,
        key: K,
        value: Vec<u8>,
        last_seq_id: Option<u64>,
    ) -> Option<sled::IVec>
    where
        K: AsRef<[u8]>,
    {
        if let Some(table_info) = self.table_map.get_mut(&name) {
            if let (Some(seq), Some(last_seq_id)) = (table_info.seq.as_mut(), last_seq_id) {
                seq.set_table_last_id(last_seq_id).ok();
            }
            let table = self.db.open_tree(&table_info.table_db_name).unwrap();
            table.insert(key, value).unwrap()
        } else {
            self.init_table(name.clone(), 0);
            let mut table_info = TableInfo::new(name.clone(), self.db.clone(), 0);
            if let (Some(seq), Some(last_seq_id)) = (table_info.seq.as_mut(), last_seq_id) {
                seq.set_table_last_id(last_seq_id).ok();
            }
            let table = self.db.open_tree(&table_info.table_db_name).unwrap();
            self.table_map.insert(name, table_info);
            table.insert(key, value).unwrap()
        }
    }

    pub fn remove<K>(&mut self, name: Arc<String>, key: K) -> Option<sled::IVec>
    where
        K: AsRef<[u8]>,
    {
        if let Some(table_info) = self.table_map.get(&name) {
            let table = self.db.open_tree(&table_info.table_db_name).unwrap();
            table.remove(key).unwrap()
        } else {
            None
        }
    }
}

impl Actor for TableManage {
    type Context = Context<Self>;
}

#[derive(Message)]
#[rtype(result = "anyhow::Result<TableManageResult>")]
pub enum TableManageAsyncCmd {
    Insert {
        table_name: Arc<String>,
        key: Vec<u8>,
        value: Vec<u8>,
    },
    Remove {
        table_name: Arc<String>,
        key: Vec<u8>,
    },
    Drop(Arc<String>),
}

#[derive(Message)]
#[rtype(result = "anyhow::Result<TableManageResult>")]
pub enum TableManageCmd {
    Set {
        table_name: Arc<String>,
        key: Vec<u8>,
        value: Vec<u8>,
        last_seq_id: Option<u64>,
    },
    Remove {
        table_name: Arc<String>,
        key: Vec<u8>,
    },
    Drop(Arc<String>),
    NextId {
        table_name: Arc<String>,
        seq_step: Option<u32>,
    },
    SetSeqId {
        table_name: Arc<String>,
        last_seq_id: u64,
    },
}

pub enum TableManageResult {
    None,
    Value(Vec<u8>),
    NextId(u64),
}

impl Handler<TableManageAsyncCmd> for TableManage {
    type Result = ResponseActFuture<Self, anyhow::Result<TableManageResult>>;

    fn handle(&mut self, msg: TableManageAsyncCmd, ctx: &mut Self::Context) -> Self::Result {
        todo!()
    }
}

impl Handler<TableManageCmd> for TableManage {
    type Result = anyhow::Result<TableManageResult>;

    fn handle(&mut self, msg: TableManageCmd, _ctx: &mut Self::Context) -> Self::Result {
        match msg {
            TableManageCmd::Set {
                table_name,
                key,
                value,
                last_seq_id,
            } => match self.insert(table_name, key, value, last_seq_id) {
                Some(v) => Ok(TableManageResult::Value(v.to_vec())),
                None => Ok(TableManageResult::None),
            },
            TableManageCmd::Remove { table_name, key } => match self.remove(table_name, key) {
                Some(v) => Ok(TableManageResult::Value(v.to_vec())),
                None => Ok(TableManageResult::None),
            },
            TableManageCmd::Drop(name) => {
                self.drop_table(&name);
                Ok(TableManageResult::None)
            }
            TableManageCmd::NextId {
                table_name,
                seq_step,
            } => match self.next_id(table_name, seq_step.unwrap_or(100)) {
                Ok(v) => Ok(TableManageResult::NextId(v)),
                Err(_) => Ok(TableManageResult::None),
            },
            TableManageCmd::SetSeqId {
                table_name,
                last_seq_id,
            } => {
                self.set_last_seq_id(table_name, last_seq_id);
                Ok(TableManageResult::None)
            }
        }
    }
}