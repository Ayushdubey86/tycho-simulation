use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use std::fmt::Debug;

use ethers::{
    providers::Middleware,
    types::{BlockId, BlockNumber, H160, H256},
};
use ethersdb::EthersDB;
use petgraph::visit::Data;
use revm::{
    db::DatabaseRef,
    interpreter::analysis::to_analysed,
    primitives::{hash_map, AccountInfo, Bytecode, Bytes, B160, B256, U256 as rU256},
    Database,
};
use revm::db::ethersdb;


/// Short-lived object that wraps an actual SimulationDB and can be passed to REVM which takes
/// ownership of it.
pub struct SharedSimulationDB<'a, DB>
where
    DB: Database,
{
    db: &'a mut SimulationDB<DB>,
}

impl<'a, DB> SharedSimulationDB<'a, DB>
where
    DB: Database,
{
    pub fn new(db: &'a mut SimulationDB<DB>) -> Self {
        Self { db }
    }
}

impl<'a, DB: Database> Database for SharedSimulationDB<'a, DB> {
    type Error = DB::Error;

    fn basic(&mut self, address: B160) -> Result<Option<AccountInfo>, Self::Error> {
        Database::basic(self.db, address)
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        Database::code_by_hash(self.db, code_hash)
    }

    fn storage(&mut self, address: B160, index: rU256) -> Result<rU256, Self::Error> {
        Database::storage(self.db, address, index)
    }

    fn block_hash(&mut self, number: rU256) -> Result<B256, Self::Error> {
        Database::block_hash(self.db, number)
    }
}

pub struct BlockHeader {
    number: u64,
    hash: H256,
    timestamp: u64,
}

pub struct StateUpdate {
    storage: Option<hash_map::HashMap<rU256, rU256>>,
    balance: Option<rU256>,
    code: Option<Bytes>,
}

pub struct SimulationDB<ExtDB: Database> {
    /// External database capable of querying data from blockchain
    external_db: ExtDB,
    /// Accounts that we had to query because we didn't expect them to be accessed during simulations
    missed_accounts: HashSet<B160>,
    /// Accounts that should not fallback to using a storage query
    mocked_accounts: HashSet<B160>,
    /// Current block
    block: Option<BlockHeader>,
}

impl<ExtDB: Database> SimulationDB<ExtDB> {
    pub fn new(external_db: ExtDB) -> Self {
        Self {
            external_db,
            missed_accounts: HashSet::new(),
            mocked_accounts: HashSet::new(),
            block: None,
        }
    }

//     fn track_miss(&mut self, address: B160) {
//         // TODO actual caching
//         if true {
//             self.missed_accounts.insert(address);
//         }
//     }
// 
//     /// Clears accounts from state that were loaded using a query
//     ///
//     /// It is recommended to call this after a new block is received,
//     /// to avoid cached state leading to wrong results.
//     pub fn clear_missed_accounts(&mut self) {
//         // for address in self.missed_accounts.iter() {
//         //     self.external_db
//         //         .accounts
//         //         .remove(address)
//         //         .expect("Inconsistency between missed_accounts and db.accounts");
//         // }
//         self.missed_accounts.clear();
//     }
// 
//     /// Sets up the code at multiple accounts.
//     ///
//     /// Allows to specify the same code for multiple accounts as is usual the
//     /// case with protocols that use factories. Can't be used for more advanced
//     /// cases e.g. if the contract uses native ETH a balance should probably be passed.
//     ///
//     /// Any account set up here is expected to be "tracked" and to receive
//     /// state updates reliably. If during simulation an account outside of the
//     /// initialised contracts is accessed, it will issue the corresponding request
//     /// to the underlying nodes to retrieve the necessary data. This data is then
//     /// cached until the next state update.
//     pub fn init_contracts(&mut self, addresses: &[B160], code: Bytes, mock: bool) {
//         let bytecode = to_analysed(Bytecode::new_raw(code));
//         for addr in addresses.iter() {
//             // let info = AccountInfo {
//             //     balance: rU256::from(0),
//             //     nonce: 0u64,
//             //     code_hash: B256::zero(),
//             //     code: Some(bytecode.clone()),
//             // };
//             // self.external_db.insert_account_info(*addr, info);
//             self.missed_accounts.insert(*addr);
//         }
//         if mock {
//             self.mocked_accounts.extend(addresses.iter());
//         }
//     }
// 
//     /// Sets up a single account
//     ///
//     /// Full control over setting up an accounts. Allows to set up EOAs as
//     /// well as smart contracts.
//     ///
//     /// If an account is mocked, it will not be allowed to query the
//     /// underlying node for any missing state.
//     pub fn init_account(&mut self, address: B160, account: AccountInfo, mock: bool) {
//         // self.external_db.insert_account_info(address, account);
//         if mock {
//             self.mocked_accounts.insert(address);
//         }
//     }
// 
//     /// Update the simulation state.
//     ///
//     /// Updates the underlying smart contract storage. Any previously missed account,
//     /// which was queried and whose state now is in the cache will be cleared.
//     ///
//     /// Returns a state update struct to revert this update.
//     pub fn update_state(
//         &mut self,
//         update: &hash_map::HashMap<B160, StateUpdate>,
//         block: BlockHeader,
//     ) -> hash_map::HashMap<B160, StateUpdate> {
//         let mut revert_updates = hash_map::HashMap::new();
//         self.external_db.block = Some(BlockId::Number(BlockNumber::Number(block.number.into())));
//         self.block = Some(block);
//         for (address, update_info) in update.iter() {
//             let mut revert_entry = StateUpdate {
//                 storage: None,
//                 balance: None,
//                 code: None,
//             };
//             if let Some(account) = self.external_db.accounts.get_mut(address) {
//                 if let Some(new_code) = &update_info.code {
//                     revert_entry.code = account.info.code.clone().map(|code| code.bytecode);
//                     account.info.code = Some(to_analysed(Bytecode::new_raw(new_code.clone())));
//                 }
// 
//                 if let Some(new_balance) = update_info.balance {
//                     revert_entry.balance = Some(account.info.balance);
//                     account.info.balance = new_balance;
//                 }
// 
//                 if let Some(storage) = &update_info.storage {
//                     let mut revert_storage = hash_map::HashMap::new();
//                     for (slot, value) in storage.iter() {
//                         if let Some(previous_value) = account.storage.insert(*slot, *value) {
//                             revert_storage.insert(*slot, previous_value);
//                         }
//                     }
//                     revert_entry.storage = Some(revert_storage);
//                 }
// 
//                 revert_updates.insert(*address, revert_entry);
//             } else {
//                 // TODO: raise a warning here about receiving an update
//                 //  for an uninitialized account
//             }
//         }
//         revert_updates
//     }
}


impl<DB: Database> Database for SimulationDB<DB> {
    type Error = DB::Error;

    fn basic(&mut self, address: B160) -> Result<Option<AccountInfo>, Self::Error> {
        // self.track_miss(address);
        Database::basic(&mut self.external_db, address)
    }

    fn code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
        panic!("Not implemented")
    }

    fn storage(&mut self, address: B160, index: rU256) -> Result<rU256, Self::Error> {
        // Note: we do only check on account level, not storage level as the existence
        //  of an account is interpreted as the account being tracked.
        // self.track_miss(address);
        // if we are accessing a mocked contract, we should now allow it to do a
        //  query as the query might return garbage, so in case we would do a query we
        //  return an empty slot instead.
        // if self.mocked_accounts.contains(&address) {
            // if let Some(db_account) = self.external_db.accounts.get(&address) {
            //     if let Some(value) = db_account.storage.get(&index) {
            //         Ok(*value)
            //     } else {
            //         Ok(rU256::ZERO)
            //     }
            // } else {
            //     Ok(rU256::ZERO)
            // }
        // } else {
            Database::storage(&mut self.external_db, address, index)
        // }
    }

    fn block_hash(&mut self, _number: rU256) -> Result<B256, Self::Error> {
        todo!()
    }
}

// // If we use SharedDB we might not need the clone trait anymore
// pub struct EthRpcDB<M: Middleware + Clone> {
//     pub client: Arc<M>,
//     pub block: Option<BlockId>,
//     pub runtime: Option<Arc<tokio::runtime::Runtime>>,
// }
// 
// impl<M: Middleware + Clone> EthRpcDB<M> {
//     /// internal utility function to call tokio feature and wait for output
//     pub fn block_on<F: core::future::Future>(&self, f: F) -> F::Output {
//         // If we get here and have to block the current thread, we really
//         // messed up indexing / filling the cache. In that case this will save us
//         // at the price of a very high time penalty.
//         match &self.runtime {
//             Some(runtime) => runtime.block_on(f),
//             None => futures::executor::block_on(f),
//         }
//     }
// }
// 
// // Unfortunately EthersDB does not implement the DatabaseRef trait
// impl<M: Middleware + Clone> DatabaseRef for EthRpcDB<M> {
//     type Error = M::Error;
// 
//     fn basic(&self, address: B160) -> Result<Option<AccountInfo>, Self::Error> {
//         println!("loading basic data {address}!");
//         let fut = async {
//             tokio::join!(
//                 self.client.get_balance(H160(address.0), None),
//                 self.client.get_transaction_count(H160(address.0), None),
//                 self.client.get_code(H160(address.0), None),
//             )
//         };
// 
//         let (balance, nonce, code) = self.block_on(fut);
// 
//         Ok(Some(AccountInfo::new(
//             rU256::from_limbs(
//                 balance
//                     .unwrap_or_else(|e| panic!("ethers get balance error: {e:?}"))
//                     .0,
//             ),
//             nonce
//                 .unwrap_or_else(|e| panic!("ethers get nonce error: {e:?}"))
//                 .as_u64(),
//             to_analysed(Bytecode::new_raw(
//                 code.unwrap_or_else(|e| panic!("ethers get code error: {e:?}"))
//                     .0,
//             )),
//         )))
//     }
// 
//     fn code_by_hash(&self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
//         panic!("Should not be called. Code is already loaded");
//         // not needed because we already load code with basic info
//     }
// 
//     fn storage(&self, address: B160, index: rU256) -> Result<rU256, Self::Error> {
//         println!("Loading storage {address}, {index}");
//         let add = H160::from(address.0);
//         let index = H256::from(index.to_be_bytes());
//         let fut = async {
//             let storage = self.client.get_storage_at(add, index, None).await.unwrap();
//             rU256::from_be_bytes(storage.to_fixed_bytes())
//         };
//         Ok(self.block_on(fut))
//     }
// 
//     fn block_hash(&self, _number: rU256) -> Result<B256, Self::Error> {
//         todo!()
//     }
// }
