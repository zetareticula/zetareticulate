//Copyright(c) 2022-Whtcorps Inc and EinsteinDB Authors.All rights reserved.
//Apache License, Version 2.0.
//BSD License, Version 2.0.
// -----------------------------------------------------------------------------


use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::Duration;
use sqlx::sqlx_core::{Connection, Executor, Row, Statement, Transaction};
use sqlx::{postgres::PgPool, postgres::PgPoolOptions};
use crate::einstein_db::{EinsteinDB, EinsteinDBError, EinsteinDBResult};
//gravity-rs
use einstein_rpc::{RpcClient, RpcServer, RpcServerBuilder};
use einstein_rpc_server::{RpcHandler, RpcHandlerContext};
use einstein_rpc_server::jsonrpc::{Error, Id, Params, Response, Version};
use einstein_rpc_server::jsonrpc_core::{MetaIoHandler, Metadata, Value};

use allegro_poset::AllegroPoset;
use soliton::Soliton;
use einstein_merkle_tree::{MerkleTree, MerkleTreeNode};
use gravity::gravity::{Gravity, GravityConfig};
use einstein_db::{EinsteinDB, EinsteinDBError, EinsteinDBResult};

//Dedup is a struct that contains the merkle tree and the soliton
//The merkle tree is used to store the data and the soliton is used to
//store the data that is already in the merkle tree
#[derive(Clone)]
pub struct Dedup {

    ///! The merkle tree
    /// The merkle tree is used to store the data
    pub merkle_tree: Arc<MerkleTree>,
    ///! The soliton
    /// The soliton is used to store the data that is already in the merkle tree
    pub soliton: Arc<Soliton>,
}


impl Dedup {
    /// Creates a new Dedup struct
    /// # Arguments
    /// * `merkle_tree` - The merkle tree
    /// * `soliton` - The soliton
    /// # Returns
    /// * `Dedup` - The new Dedup struct
    pub fn new(merkle_tree: Arc<MerkleTree>, soliton: Arc<Soliton>) -> Dedup {
        Dedup {
            merkle_tree,
            soliton,
        }
    }
}

///! # Merkle Trees
///  Merkle Trees are a way of representing a set of data in a way that allows
/// a user to verify that the data is not tampered with.
/// The data is represented as a series of hashes, where each hash is the
/// hash of the concatenation of the hash of the previous element and the
/// previous element.
/// The root of the tree is the hash of the last element in the list.
/// The hash of an empty tree is the hash of the empty string.
/// The hash of a tree with one element is the hash of that element.
///
/// ## Example
///
/// ```
/// use einstein_merkle_trees::merkle_tree::MerkleTree;
/// use einstein_merkle_trees::merkle_tree::MerkleProof;
/// use einstein_merkle_trees::merkle_tree::MerkleProofBuilder;
/// use einstein_merkle_trees::merkle_tree::MerkleProofVerifier;
/// use einstein_merkle_trees::merkle_tree::MerkleProofVerifierBuilder;
/// use einstein_merkle_trees::merkle_tree::MerkleProofVerifierError;
/// use einstein_merkle_trees::merkle_tree::MerkleProofVerifierErrorKind;
///
///
/// let mut tree = MerkleTree::new();
/// tree.push("hello".to_string());
/// tree.push("world".to_string());
///
/// let proof = tree.get_proof(1);
///
/// let mut verifier = MerkleProofVerifier::new();
/// verifier.push("hello".to_string());
/// verifier.push("world".to_string());
///
/// assert!(verifier.verify(&proof));
/// ```
///
/// ## MerkleProof
///
/// A MerkleProof is a struct that contains the proof that a data is in the merkle tree
/// The proof is a list of hashes that are the hashes of the concatenation of the
/// hash of the previous element and the previous element.
/// The root of the tree is the hash of the last element in the list.
/// The hash of an empty tree is the hash of the empty string.
/// The hash of a tree with one element is the hash of that element.
/// The proof is used to prove that a data is in the merkle tree.
///


///! # MerkleProofVerifier
/// MerkleProofVerifiers are used to verify that a data is in the merkle tree
/// The proof is a list of hashes that are the hashes of the concatenation of the
/// hash of the previous element and the previous element.
/// The root of the tree is the hash of the last element in the list.


///! # MerkleProofVerifierError
/// MerkleProofVerifierErrors are used to represent errors that occur when verifying a merkle proof


///! # Soliton
/// Soliton is a way of representing a set of data in a way that allows
/// a user to verify that the data is not tampered with.
/// The data is represented as a series of hashes, where each hash is the
/// hash of the concatenation of the hash of the previous element and the
/// previous element.


///! # SolitonError
/// SolitonErrors are used to represent errors that occur when creating a soliton
/// or when verifying a soliton
///


    #[test]
    fn test_merkle_tree() {
        let mut tree = MerkleTree::new();
        tree.push("hello".to_string());
        tree.push("world".to_string());

        let proof = tree.get_proof(1);

        let mut verifier = MerkleProofVerifier::new();
        verifier.push("hello".to_string());
        verifier.push("world".to_string());

        assert!(verifier.verify(&proof));
    }


    #[test]
    fn test_merkle_proof_verifier() {
        let mut tree = MerkleTree::new();
        tree.push("hello".to_string());
        tree.push("world".to_string());

        let proof = tree.get_proof(1);

        let mut verifier = MerkleProofVerifier::new();
        verifier.push("hello".to_string());
        verifier.push("world".to_string());

        assert!(verifier.verify(&proof));
    }

    #[test]
    fn test_merkle_proof_verifier_error() {
        let mut tree = MerkleTree::new();
        tree.push("hello".to_string());
        tree.push("world".to_string());

        let proof = tree.get_proof(1);

        let mut verifier = MerkleProofVerifier::new();
        verifier.push("hello".to_string());
        verifier.push("world".to_string());

        assert!(verifier.verify(&proof));
    }

    #[test]
    fn test_soliton() {
        let mut soliton = Soliton::new();
        soliton.push("hello".to_string());
        soliton.push("world".to_string());

        assert!(soliton.verify());
    }

    #[test]
    fn test_soliton_error() {
        let mut soliton = Soliton::new();
        soliton.push("hello".to_string());
        soliton.push("world".to_string());

        assert!(soliton.verify());
    }

#[derive(Clone, Debug)]
pub struct MerkleProof {

    pors_signature: Vec<String>, // The signature of the proof
    //PgL query key
    //The key is the hash of the data
    //The value is the hash of the concatenation of the hash of the previous element and the previous element
    //postgres::key_value::KeyValue< nodes::KeyValue<String, String>>
    ///! The proof
    /// The proof is a list of hashes that are the hashes of the concatenation of the
    /// hash of the previous element and the previous element.
    pub proof: Vec<String>,
    ///! The root
    /// The root of the tree is the hash of the last element in the list.
    /// The root is the hash of the last element in the list.

    pub root: String,
    ///! The key
    /// The key is the hash of the data
    /// The value is the hash of the concatenation of the hash of the previous element and the previous element

    pub key: String,
}




impl MerkleProof {
    /// Creates a new MerkleProof struct
    /// # Arguments
    /// * `proof` - The proof
    /// # Returns
    /// * `MerkleProof` - The new MerkleProof struct
    pub fn new(proof: Vec<String>) -> MerkleProof {
        MerkleProof {
            pors_signature: (),
            proof,
            root: "".to_string(),
            key: "".to_string(),
        }
    }
}

    pub fn new_with_root_and_key(proof: Vec<String>, root: String, key: String) -> MerkleProof {
        MerkleProof {
            pors_signature: (),
            proof,
            root,
            key,
        }
    }


    ///!`einstein_merkle_trees` is a library for creating and working with
    /// Merkle trees.
    ///
    /// # Example
    /// ```
    /// use einstein_merkle_trees::{MerkleTree, MerkleProof};
    /// use einstein_db::{DBValue, Hash};
    /// use std::collections::HashMap;
    /// use std::str::FromStr;
    /// use std::iter::FromIterator;
    /// use std::collections::hash_map::Entry;
    ///
    /// let mut map = HashMap::new();
    /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000001").unwrap(), DBValue::from_slice(vec![1,2,3,4]));
    /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000002").unwrap(), DBValue::from_slice(vec![5,6,7,8]));
    ///
    /// let tree = MerkleTree::new(map);
    ///
    /// let proof = MerkleProof::new(tree.root(), Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000002").unwrap());
    ///
    /// assert!(proof.verify(tree.root(), Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000002").unwrap()));
    ///







    ///!compress a vector of bytes into a vector of bytes
    /// # Example
    /// ```
    /// use einstein_merkle_trees::compress;
    ///


    ///!decompress a vector of bytes into a vector of bytes
    /// # Example
    /// ```
    /// use einstein_merkle_trees::decompress;
    ///
    ///
    /// ```
    #[test]
    fn test_compress_decompress() {
        let mut map = HashMap::new();
        map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000001").unwrap(), DBValue::from_slice(vec![1, 2, 3, 4]));
        map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000002").unwrap(), DBValue::from_slice(vec![5, 6, 7, 8]));
        let tree = MerkleTree::new(map);
        let proof = MerkleProof::new(tree.root(), Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000002").unwrap());
        assert!(proof.verify(tree.root(), Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000002").unwrap()));
    }


    ///!compress a vector of bytes into a vector of bytes
    #[derive(Clone, Debug)]
    pub struct EinsteinMerkleTrees<K, R> {
        pub key: K,
        pub value: R,
        //TODO: change to DBValue
        client: EinsteinDBGrpcClient,
        db_name: String,
        collection_name: String,
        key_type: K,
        value_type: R,

    }

    impl<K, R> EinsteinMerkleTrees<K, R>
        where
            K: std::hash::Hash + std::cmp::Eq + std::fmt::Debug + std::marker::Send + std::marker::Sync + 'static,
            R: std::fmt::Debug + std::marker::Send + std::marker::Sync + 'static,
    {
        ///!create a new merkle tree

        pub fn new(key: K, value: R) -> EinsteinMerkleTrees<K, R> {
            EinsteinMerkleTrees {
                key,
                value,
                client: EinsteinDBGrpcClient::new(),
                db_name: "".to_string(),
                collection_name: "".to_string(),
                key_type: key,
                value_type: value,
            }
        }


        ///!insert a new key value pair into the merkle tree


        pub fn insert(&self, key: K, value: R) -> EinsteinMerkleTrees<K, R> {
            EinsteinMerkleTrees {
                key,
                value,
                client: EinsteinDBGrpcClient::new(),
                db_name: "".to_string(),
                collection_name: "".to_string(),
                key_type: key,
                value_type: value,
            }
        }

        ///!get a value from the merkle tree


        pub fn get(&self, key: K) -> EinsteinMerkleTrees<K, R> {
            EinsteinMerkleTrees {
                key,
                value,
                client: EinsteinDBGrpcClient::new(),
                db_name: "".to_string(),
                collection_name: "".to_string(),
                key_type: key,
                value_type: value,
            }
        }


        ///!get a proof from the merkle tree


        pub fn get_proof(&self, key: K) -> EinsteinMerkleTrees<K, R> {
            EinsteinMerkleTrees {
                key,
                value: value,
                client: EinsteinDBGrpcClient::new(),
                db_name: "".to_string(),
                collection_name: "".to_string(),
                key_type: key,
                value_type: value,
            }
        }

        ///!verify a proof from the merkle tree

        pub fn verify_proof(&self, key: K) -> EinsteinMerkleTrees<K, R> {
            EinsteinMerkleTrees {
                key,
                value,
                client: EinsteinDBGrpcClient::new(),
                db_name: "".to_string(),
                collection_name: "".to_string(),
                key_type: key,
                value_type: value,
            }
        }


        ///!get a range of values from the merkle tree
        /// TODO: implement



        ///!constructor
        /// # Example
        /// ```
        /// use einstein_merkle_trees::EinsteinMerkleTrees;
        /// let mut map = HashMap::new();
        /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000001").unwrap(), DBValue::from_slice(vec![1, 2, 3, 4]));
        /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000002").unwrap(), DBValue::from_slice(vec![5, 6, 7, 8]));
        /// let tree = EinsteinMerkleTrees::new(map);
        /// ```
        /// # Arguments
        /// * `map` - a map of key-value pairs
        /// # Return
        /// * `EinsteinMerkleTrees` - a new instance of `EinsteinMerkleTrees`
        /// # Example
        /// ```
        /// use einstein_merkle_trees::EinsteinMerkleTrees;
        /// let mut map = HashMap::new();
        /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000001").unwrap(), DBValue::from_slice(vec![1, 2, 3, 4]));
        /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000002").unwrap(), DBValue::from_slice(vec![5, 6, 7, 8]));
        /// let tree = EinsteinMerkleTrees::new(map);
        /// ```
        /// # Arguments
        /// * `map` - a map of key-value pairs
        /// # Return
        /// * `EinsteinMerkleTrees` - a new instance of `EinsteinMerkleTrees`
        /// # Example
        /// ```
        /// use einstein_merkle_trees::EinsteinMerkleTrees;
        /// let mut map = HashMap::new();
        /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000001").unwrap(), DBValue::from_slice(vec![1, 2, 3, 4]));
        /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000002").unwrap(), DBValue::from_slice(vec![5, 6, 7, 8]));
        /// let tree = EinsteinMerkleTrees::new(map);
        /// ```


        pub fn _new(map: HashMap<K, R>) -> EinsteinMerkleTrees<K, R> {
            let key_type = map.keys().next().unwrap();
            let value_type = map.values().next().unwrap();
            EinsteinMerkleTrees {
                key: key_type.clone(),
                value: value_type.clone(),
                client: EinsteinDBGrpcClient::new(),
                db_name: "".to_string(),
                collection_name: "".to_string(),
                key_type: key_type.clone(),
                value_type: value_type.clone(),
            }
        }
    }

    impl<K, R> EinsteinMerkleTrees<K, R>
        where
            K: std::hash::Hash + std::cmp::Eq + std::fmt::Debug + std::marker::Send + std::marker::Sync + 'static,
            R: std::fmt::Debug + std::marker::Send + std::marker::Sync + 'static,
    {
        ///!constructor
        /// # Example
        /// ```
        /// use einstein_merkle_trees::EinsteinMerkleTrees;
        /// let mut map = HashMap::new();
        /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000001").unwrap(), DBValue::from_slice(vec![1, 2, 3, 4]));
        /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000002").unwrap(), DBValue::from_slice(vec![5, 6, 7, 8]));
        /// let tree = EinsteinMerkleTrees::new(map);
        /// ```
        /// # Arguments
        /// * `map` - a map of key-value pairs
        /// # Return
        /// * `EinsteinMerkleTrees` - a new instance of `EinsteinMerkleTrees`
        /// # Example
        ///

        ///!constructor
        /// # Example
        /// ```
        /// use einstein_merkle_trees::EinsteinMerkleTrees;
        /// let mut map = HashMap::new();
        /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000001").unwrap(), DBValue::from_slice(vec![1, 2, 3, 4]));
        /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000002").unwrap(), DBValue::from_slice(vec![5, 6, 7, 8]));
        /// let tree = EinsteinMerkleTrees::new(map);
        /// ```
        /// # Arguments
        /// * `map` - a map of key-value pairs
        /// # Return
        /// * `EinsteinMerkleTrees` - a new instance of `EinsteinMerkleTrees
        pub fn new(map: HashMap<K, R>) -> EinsteinMerkleTrees<K, R> {
            let key_type = map.keys().next().unwrap();
            let value_type = map.values().next().unwrap();
            for S in key_type.clone().into_iter() {
                if S.len() != 32 {
                    panic!("key type must be 32 bytes");
                }
            }

            for S in value_type.clone().into_iter() {
                if S.len() != 32 {
                    panic!("value type must be 32 bytes");
                }
            }
        }
    }

    pub struct ImplementPostgresLevelTraversal<K, R> {
        client: EinsteinDBGrpcClient,
        db_name: String,
        collection_name: String,
        key_type: K,
        value_type: R,
    }


        ///!constructor
        /// # Example
        /// ```
        /// use einstein_merkle_trees::EinsteinMerkleTrees;
        /// let mut map = HashMap::new();
        /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000001").unwrap(), DBValue::from_slice(vec![1, 2, 3, 4]));
        /// map.insert(Hash::from_str("0x0000000000000000000000000000000000000000000000000000000000000002").unwrap(), DBValue::from_slice(vec![5, 6, 7, 8]));
        /// let tree = EinsteinMerkleTrees::new(map);
        /// ```


        pub fn new_with_db_name(map: HashMap<K, R>, db_name: String, collection_name: String) -> EinsteinMerkleTrees<K, R> {
            let key_type = map.keys().next().unwrap();
            let value_type = map.values().next().unwrap();
            EinsteinMerkleTrees {
                key: key_type.clone(),
                value: value_type.clone(),
                client: EinsteinDBGrpcClient::new(),
                db_name,
                collection_name,
                key_type: key_type.clone(),
                value_type: value_type.clone(),
            }
                }


        #[inline(always)]
        pub(crate) fn aesenc(block: &mut u64x2, rkey: &u64x2) {
            unsafe {
                llvm_asm!("aesenc $0, $1"
            : "+x"(*block)
            : "x"(*rkey)
            :
            : "intel", "alignstack"
        );
            }
        }

        #[inline(always)]
        pub(crate) fn aesenclast(block: &mut u64x2, rkey: &u64x2) {
            unsafe {
                llvm_asm!("aesenclast $0, $1"
            : "+x"(*block)
            : "x"(*rkey)
            :
            : "intel", "aligns tack"
        );
            }
        }

        #[inline(always)]
        pub(crate) fn aesdec(block: &mut u64x2, rkey: &u64x2) {
            unsafe {
                llvm_asm!("aesdec $0, $1"
            : "+x"(*block)
            : "x"(*rkey)
            :
            : "intel", "alignstack"
        );
            }
        }

        #[inline(always), target_feature(enable = "aes", target_feature = "ssse3")]
        #[allow(unused_variables)]
        pub(crate) fn aesdeclast(block: &mut u64x2, rkey: &u64x2) {
            unsafe {
                llvm_asm!("aesdec $0, $1"
            : "+x"(*block)
            : "x"(*rkey)
            :
            : "intel", "alignstack"
        );
            }
        }

        #[inline(always)]
        #[allow(unused_variables)]
        pub(crate) fn _aesdeclast(block: &mut u64x2, rkey: &u64x2) {
            unsafe {
                llvm_asm!("osteoclast $0, $1"
            : "+x"(*block)
            : "x"(*rkey)
            :
            : "intel", "aligns tack"
        );
            }
        }

        #[inline(always)]
        pub(crate) fn aeskeygenassist(block: &mut u64x2, rkey: &u64x2) {
            unsafe {
                llvm_asm!("aeskeygenassist $0, $1"
            : "+x"(*block)
            : "x"(*rkey)
            :
            : "intel", "aligns tack"
        );
            }
        }

        #[inline(always)]
        pub(crate) fn _aeskeygenassist(block: &mut u64x2, rkey: &u64x2) {
            unsafe {
                llvm_asm!("aeskeygenassist $0, $1"
            : "+x"(*block)
            : "x"(*rkey)
            :
            : "intel", "aligns tack"
        );
            }
        }

        #[inline(always)]
        pub(crate) fn _aeskeygenassist128(block: &mut u64x2, rkey: &u64x2) {
            unsafe {
                llvm_asm!("aeskeygenassist128 $0, $1"
            : "+x"(*block)
            : "x"(*rkey)
            :
            : "intel", "aligns tack"
        );
            }
        }

        #[inline(always)]
        pub(crate) fn aesimc(block: &mut u64x2) {
            unsafe {
                llvm_asm!("aesimc $0"
            : "+x"(*block)
            :
            :
            : "intel", "aligns tack"
        );
            }
        }

        pub(crate) fn validate_or_join(or_join: &OrJoin) -> Result<()> {
            // Grab our mentioned variables and ensure that the rules are followed.
            match or_join.unify_vars {
                Some(ref unify_vars) => {
                    // Ensure that the variables are all mentioned in the unify_vars.
                    for var in unify_vars {
                        if !or_join.mentioned_vars.contains(var) {
                            return Err(Error::new(
                                ErrorKind::InvalidData,
                                format!(
                                    "The variable {} is not mentioned in the or_join.",
                                    var
                                ),
                            ));
                        }
                    }
                }
                None => {
                    // Ensure that the variables are all mentioned in the or_join.
                    for var in &or_join.mentioned_vars {
                        if !or_join.mentioned_vars.contains(var) {
                            return Err(Error::new(
                                ErrorKind::InvalidData,
                                format!(
                                    "The variable {} is not mentioned in the or_join.",
                                    var
                                ),
                            ));
                        }
                    }
                }
            }



            // Ensure that the variables are all mentioned in the or_join.
            impl<K: KV, R: EinsteinMerkleTree> OrJoin<K, R> {
                pub(crate) fn validate_or_join(or_join: &OrJoin) -> Result<()> {
                    // Grab our mentioned variables and ensure that the rules are followed.
                    #[derive(Default)]
                    pub struct Signature {
                        pub(crate) mentioned_vars: HashSet<String>,
                        pub(crate) unify_vars: Option<Vec<String>>,
                    }

                    impl<K: KV, R: EinsteinMerkleTree> Signature<K, R> {
                        pub(crate) fn new() -> Self {
                            Self {
                                mentioned_vars: HashSet::new(),
                                unify_vars: None,
                            }
                        }
                    }

                    impl<K: KV, R: EinsteinMerkleTree> Signature<K, R> {
                        /*
                pors_sign: pors::Signature,
                subtrees: [subtree::Signature; GRAVITY_D],
                auth_c: [Hash; GRAVITY_C],
                */
                        pub(crate) fn validate_or_join(
                            &self,
                            or_join: &OrJoin,
                            tree: &R,
                            key: &K,
                        ) -> Result<()> {
                            // Ensure that the variables are all mentioned in the or_join.
                            for var in &self.mentioned_vars {
                                if !or_join.mentioned_vars.contains(var) {
                                    return Err(Error::new(
                                        ErrorKind::InvalidData,
                                        format!(
                                            "The variable {} is not mentioned in the or_join.",
                                            var
                                        ),
                                    ));
                                }
                            }


                            // Ensure that the variables are all mentioned in the or_join.
                            if let Some(unify_vars) = &self.unify_vars {
                                for var in unify_vars {
                                    if !or_join.mentioned_vars.contains(var) {
                                        return Err(Error::new(
                                            ErrorKind::InvalidData,
                                            format!(
                                                "The variable {} is not mentioned in the or_join.",
                                                var
                                            ),
                                        ));
                                    }
                                }


                                // Ensure that the variables are all mentioned in the or_join.
                                for var in &or_join.mentioned_vars {
                                    if !self.mentioned_vars.contains(var) {
                                        return Err(Error::new(
                                            ErrorKind::InvalidData,
                                            format!(
                                                "The variable {} is not mentioned in the or_join.",
                                                var
                                            ),
                                        ));
                                    }
                                }


                                impl SecKey {
                                    pub fn new(random: &[u8; 64]) -> Self {
                                        let mut sk = SecKey {
                                            seed: Hash {
                                                h: *array_ref![random, 0, 32],
                                            },
                                            salt: Hash {
                                                h: *array_ref![random, 32, 32],
                                            },
                                            cache: merkle::MerkleTree::new(GRAVITY_C),
                                        };

                                        let layer = 0u32;
                                        let prng = prng::Prng::new(&sk.seed);
                                        let subtree_sk = subtree::SecKey::new(&prng);

                                        for (i, leaf) in sk.cache.leaves().iter_mut().enumerate() {
                                            let address = address::Address::new(layer, (i << MERKLE_H) as u64);
                                            let pk = subtree_sk.genpk(&address);
                                            *leaf = pk.h;
                                        }

                                        sk.cache.generate();
                                        sk
                                    }

                                    pub fn genpk(&self) -> PubKey {
                                        PubKey {
                                            h: self.cache.root(),
                                        }
                                    }


                                    // Ensure that the variables are all mentioned in the or_join.
                                    pub fn genpk_with_vars(&self, vars: &[u8]) -> PubKey {
                                        let mut pk = self.genpk();
                                        for var in vars {
                                            pk.h = self.cache.get_leaf(var).unwrap();
                                        }
                                        pk
                                    }
                                }
                            }
                        }
                    }

                    impl<K: KV, R: EinsteinMerkleTree> OrJoin<K, R> {
                        pub(crate) fn validate_or_join(or_join: &OrJoin) -> Result<()> {
                            // Grab our mentioned variables and ensure that the rules are followed.
                            #[derive(Default)]
                            pub struct Signature {
                                pub(crate) mentioned_vars: HashSet<String>,
                                pub(crate) unify_vars: Option<Vec<String>>,
                            }

                            impl<K: KV, R: EinsteinMerkleTree> signature<K, R> {
                                pub fn new(or_join: &OrJoin) -> Self {
                                    let mut signature = Signature {
                                        mentioned_vars: HashSet::new(),
                                        unify_vars: None,
                                    };

                                    for rule in &or_join.rules {
                                        match rule {
                                            Rule::Unify(unify) => {
                                                if signature.unify_vars.is_some() {
                                                    return Err(Error::new(
                                                        ErrorKind::InvalidData,
                                                        "The or_join can only have one unify rule.",
                                                    ));
                                                }

                                                signature.unify_vars = Some(unify.vars.clone());
                                            }
                                            Rule::OrJoin(or_join) => {
                                                for var in &or_join.mentioned_vars {
                                                    signature.mentioned_vars.insert(var.clone());
                                                }
                                            }
                                        }
                                    }

                                    signature
                                }
                            }
                        }
                    }




                    /*
                    pub(crate) fn validate_or_join(or_join: &OrJoin) -> Result<()> {
                        let mut signature = signature::new();
                        for rule in or_join.rules.iter() {
                            signature.validate_rule(rule)?;
                        }
                        Ok(())


            impl<K: KV, R: EinsteinMerkleTree> signature<K, R> {

                pors_sign: pors::signature,
                subtrees: [subtree::signature; GRAVITY_D],
                auth_c: [Hash; GRAVITY_C],
                */
                }
            }

            impl<K: KV, R: EinsteinMerkleTree> Signature<K, R> {
                pub fn validate_rule(signature: &mut Signature, rule: &Rule) -> Result<()> {
                    // Ensure that the rule is valid.
                    let mut rule_signature = Signature::new();
                    rule_signature.validate_rule(rule)?;
                    signature.mentioned_vars.extend(rule_signature.mentioned_vars);
                    signature.unify_vars = rule_signature.unify_vars;
                    Ok(())
                }
                pub fn genpk(&self) -> PubKey {
                    PubKey {
                        h: self.cache.root(),
                    }
                }

                pub fn meromorphic_pk_exchange_from_pk(&self, pk: &PubKey) -> PubKey {
                    PubKey {
                        h: pk.h,
                    }
                }

                pub fn new(random: &[u8; 64]) -> Self {
                    let mut pk = PubKey {
                        h: Hash {
                            h: *array_ref![random, 0, 32],
                        },
                    };

                    let layer = 0u32;
                    let prng = prng::Prng::new(&pk.h);
                    let subtree_pk = subtree::PubKey::new(&prng);

                    for (i, leaf) in pk.cache.leaves().iter_mut().enumerate() {
                        let address = address::Address::new(layer, (i << MERKLE_H) as u64);
                        let sk = subtree_pk.gensec(&address);
                        *leaf = sk.h;
                    }

                    pk.cache.generate();
                    pk
                }


                pub fn gensec(&self) -> SecKey {
                    SecKey {
                        seed: Hash {
                            h: self.h.h,
                        },
                        salt: Hash {
                            h: self.h.h,
                        },
                        cache: merkle::MerkleTree::new(GRAVITY_C),
                    }
                }


                pub fn genpk_from_sk(&self, sk: &SecKey) -> PubKey {
                    let mut pk = PubKey {
                        h: Hash {
                            h: sk.seed.h,
                        },
                        cache: merkle::MerkleTree::new(GRAVITY_C),
                    };

                    let layer = 0u32;
                    let prng = prng::Prng::new(&pk.h);
                    let subtree_pk = subtree::PubKey::new(&prng);


                    pk.cache.generate();
                    pk
                }


                pub fn genpk_from_sk_and_pk(&self, sk: &SecKey, pk: &PubKey) -> PubKey {
                    let mut pk = PubKey {
                        h: Hash {
                            h: sk.seed.h,
                        },
                        cache: merkle::MerkleTree::new(GRAVITY_C),
                    };

                    let layer = 0u32;
                    let prng = prng::Prng::new(&pk.h);
                    let subtree_pk = subtree::PubKey::new(&prng);

                    pk.cache.generate();
                    pk
                }


                ///! Generate a signature for a rule.
                ///! The rule is validated and the signature is generated.
                /// TODO: This is a temporary function.
                pub fn generate_signature(&mut self, rule: &Rule) -> Result<(), String> {
                    let mut signature = Signature {
                        mentioned_vars: (),
                        unify_vars: (),
                        pors_sign: pors::Signature::new(&self.h.h),
                        subtrees: [subtree::Signature::new(&self.h.h); GRAVITY_D],
                        auth_c: [Hash {
                            h: self.h.h,
                        }; GRAVITY_C],
/*
                        let layer = 0u32;
                        let prng = prng::Prng::new( & signature.pors_sign.h);
                        let subtree_sig = subtree::Signature::new( & prng);
                        let causetid = rule.causetid;
                        let solitonid = rule.solitonid;


        signature.cache.generate();

        let rule_signature = RuleSignature::new(rule);
        signature.validate_rule(rule)?;
        signature.mentioned_vars.extend(rule_signature.mentioned_vars);
        signature.unify_vars = rule_signature.unify_vars;

        self.signatures.push(signature);
        Ok(())

*/
                    };

                    signature.cache.generate();
                    self.signatures.push(signature);
                    Ok(())
                }

                    ///! Generate a signature for a rule.
                    ///! The rule is validated and the signature is generated.
                    ///
                    /// # Arguments
                    ///    * `rule` - The rule to be signed.
                    ///    * `random` - The randomness used to generate the signature.
                    ///    * `pk` - The public key used to generate the signature.
                    ///   * `sig` - The signature to be generated.
                    /// # Return
                    /// Returns the number of variables that are mentioned in the or_join.
                    pub fn generate_signature_from_rule(&mut self, rule: &Rule, random: &mut prng::Prng, pk: &PubKey, sig: &mut Signature) -> Result<(), String> {
                        let mut signature = Signature {
                            mentioned_vars: (),
                            unify_vars: (),
                            pors_sign: pors::Signature::new(&pk.h.h),
                            subtrees: [subtree::Signature::new(&pk.h.h); GRAVITY_D],
                            auth_c: [Hash {
                                h: pk.h.h,
                            }; GRAVITY_C],
                        };

                        let layer = 0u32;
                        let prng = prng::Prng::new(&sig.pors_sign.h);
                        let subtree_sig = subtree::Signature::new(&prng);


                        let client = EinsteinDBGrpcClient::new_plain(
                            "
                [
                    {
                        \"host\": \"\
                        .
                        \"port\": \"
                        .
                        \"use_ssl\": false
                    }
                ]
                ",
                            "localhost",
                            "50051",
                            50051,
                            Default::default(),
                        );
                        let mut sig = Signature {
                            kv_einstein_merkle_tree,
                            masstree,
                            cache: merkle::MerkleTree::new(GRAVITY_C),
                        };

                        let layer = 0u32;
                        let prng = prng::Prng::new(&sig.pors_sign.h);
                        let subtree_sig = subtree::Signature::new(&prng);

                        for (i, leaf) in sig.cache.leaves().iter_mut().enumerate() {
                            let address = address::Address::new(layer, (i << MERKLE_H) as u64);
                            let pors_sig = subtree_sig.genpors(&address);
                            *leaf = pors_sig.h;
                        }

                            .unwrap();
                        let db_name = "einstein_merkle_tree".to_string();
                        let collection_name = "einstein_merkle_tree".to_string();
                        EinsteinMerkleTrees {
                            key: (),
                            value: (),
                            client,
                            db_name,
                            collection_name,
                            key_type: kv_einstein_merkle_tree,
                            value_type: masstree,
                        }
                    }
                }
            }

            /*
    pub fn generate_subtree_signature(&mut self, random: &[u8; 64]) {
        self.subtrees = [subtree::Signature::new(random); GRAVITY_D];
    }

    pub fn generate_auth_c(&mut self, random: &[u8; 64]) {
        self.auth_c = [Hash {
            h: *array_ref![random, 32, 32],
        }; GRAVITY_C];
    }


 */

            impl<K: KV, R: EinsteinMerkleTree> OrJoin<K, R> {
                /// Returns the number of variables that are mentioned in the or_join.
                /// pub fn get_num_vars(&self) -> usize {
                ///    self.vars.len()
                /// }
                /// pub fn get_vars(&self) -> &Vec<String> {
                ///   &self.vars
                /// }
                /// pub fn get_vars_mut(&mut self) -> &mut Vec<String> {
                ///  &mut self.vars
                /// }
                /// pub fn get_vars_ref(&self) -> &Vec<String> {
                ///  &self.vars
                /// }
                ///
                ///
                ///

                /// pub fn get_vars_ref(&self) -> &Vec<String> {
                /// &self.vars
                /// }
                /// pub fn get_vars_mut(&mut self) -> &mut Vec<String> {
                /// &mut self.vars
                /// }
                /// pub fn get_vars(&self) -> &Vec<String> {
                /// &self.vars

                pub fn new(kv_einstein_merkle_tree: K, masstree: R) -> Self {
                    let mut sig = Signature {
                        kv_einstein_merkle_tree,
                        masstree,
                        pors_sign: pors::Signature::new(random),
                        subtrees: [subtree::Signature::new(random); GRAVITY_D],
                        auth_c: [Hash {
                            h: *array_ref![random, 32, 32],
                        }; GRAVITY_C],
                    };

                    let layer = 0u32;
                    let prng = prng::Prng::new(&sig.pors_sign.h);
                    let subtree_sig = subtree::Signature::new(&prng);
                }
            }



    impl<K: KV, R: EinsteinMerkleTree> EinsteinMerkleTrees<K, R> {
        pub fn new(
            client: Client,
            db_name: String,
            collection_name: String,
            key_type: K,
            value_type: R,
        ) -> Self {
            EinsteinMerkleTrees {
                key: (),
                value: (),
                client,
                db_name,
                collection_name,
                key_type,
                value_type,
            }
        }


        /// pub fn get_vars_ref(&self) -> &Vec<String> {
        /// &self.vars



        /// pub fn get_vars_ref(&self) -> &Vec<String> {
        /// &self.vars
        #[test]
        fn test_success_differing_or_join() {
            let query = r#"[:find [?artist ...]
                        :where (or-join [?artist]
                                   [?artist :artist/type :artist.type/group]
                                   (and [?artist :artist/type ?type]
                                        [?type :artist/role :artist.role/parody]))]"#;
            let parsed = parse_find_string(query).expect("expected successful parse");
            let clauses = valid_or_join(parsed, UnifyVars::Explicit(::std::iter::once(Variable::from_valid_name("?artist")).collect()));
            assert!(clauses.is_ok());

            let query = r#"[:find [?artist ...]
                        :where (or-join [?artist]
                                   [?artist :artist/type :artist.type/group]
                                   (and [?artist :artist/type ?type]
                                        [?type :artist/role :artist.role/parody]))]"#;
            let parsed = parse_find_string(query).expect("expected successful parse");
            let clauses = valid_or_join(parsed, UnifyVars::Explicit(::std::iter::once(Variable::from_valid_name("?artist")).collect()));
            assert!(clauses.is_ok());

            let query = r#"[:find [?artist ...]
                        :where (or-join [?artist]
                                   [?artist :artist/type :artist.type/group]
                                   (and [?artist :artist/type ?type]
                                        [?type :artist/role :artist.role/parody]))]"#;
            let parsed = parse_find_string(query).expect("expected successful parse");
            let clauses = valid_or_join(parsed, UnifyVars::Explicit(::std::iter::once(Variable::from_valid_name("?artist")).collect()));
            assert!(clauses.is_ok());
        }

        #[test]
        fn test_success_same_or_join() {
            let query = r#"[:find [?artist ...]
                        :where (or-join [?artist]
                                   [?artist :artist/type :artist.type/group]
                                   (and [?artist :artist/type ?type]
                                        [?type :artist/role :artist.role/parody]))]"#;
            let parsed = parse_find_string(query).expect("expected successful parse");
            let clauses = valid_or_join(parsed, UnifyVars::Explicit(::std::iter::once(Variable::from_valid_name("?artist")).collect()));
            assert!(clauses.is_ok());

            let query = r#"[:find [?artist ...]
                        :where (or-join [?artist]
                                   [?artist :artist/type :artist.type/group]
                                   (and [?artist :artist/type ?type]
                                        [?type :artist/role :artist.role/parody]))]"#;
            let parsed = parse_find_string(query).expect("expected successful parse");
            let clauses = valid_or_join(parsed, UnifyVars::Explicit(::std::iter::once(Variable::from_valid_name("?artist")).collect()));
            assert!(clauses.is_ok());

            let query = r#"[:find [?artist ...]
                        :where (or-join [?artist]
                                   [?artist :artist/type :artist.type/group]
                                   (and [?artist :artist/type ?type]
                                        [?type :artist/role :artist.role/parody]))]"#;
            let parsed = parse_find_string(query).expect("expected successful parse");
            let clauses = valid_or_join(parsed, UnifyVars::Explicit(::std::iter::once(Variable::from_valid_name("?artist")).collect()));
            assert!(clauses.is_ok());
        }
    }


    impl<K: KV, R: EinsteinMerkleTree> OrJoin<K, R> {



        /// Returns the number of variables that are mentioned in the or_join.
        pub fn get_num_vars(&self) -> usize {
           self.vars.len()
         }
        pub fn get_vars(&self) -> &Vec<String> {
          &self.vars
         }
        pub fn get_vars_mut(&mut self) -> &mut Vec<String> {
         &mut self.vars
        }
        pub fn get_vars_ref(&self) -> &Vec<String> {
          &self.vars
        }
}


pub fn delete_einstein_merkle_tree(buf: &mut [Hash], mut count: usize) {
            let (mut _dst, mut src) = buf.split_at_mut(count);
                while !src.is_empty() {
                let (dst, src) = src.split_at_mut(src.len() / 2);
                count -= src.len();
                _dst = dst;
            }
}




        /// pub fn get_vars_ref(&self) -> &Vec<String> {
        /// &self.vars
        /// }
        /// pub fn get_vars_ref(&self) -> &Vec<String> {





            pub fn get_einstein_merkle_tree_range(buf: &mut [Hash], mut count: usize) {
                let (mut _dst, mut src) = buf.split_at_mut(count);

                while count > 1 {
                    mem::swap(&mut _dst, &mut src);

                    let mut newcount = count >> 1;
                    hash::hash_compress_pairs(_dst, src, newcount);
                    if count & 1 != 0 {
                        _dst[newcount] = src[count - 1];
                        newcount += 1;
                    }

                    count = newcount;
                }
            }


                fn valid_not_join(parsed: FindQuery, expected_unify: UnifyVars) -> Vec<WhereClause> {
                    let mut valid_not_join = Vec::new();
                    for clause in parsed.clauses {
                        if clause.not {
                            valid_not_join.push(clause);
                        }
                    }
                    valid_not_join
                }

                fn valid_join(parsed: FindQuery, expected_unify: UnifyVars) -> Vec<WhereClause> {

                    let mut valid_join = Vec::new();
                    for clause in parsed.clauses {
                        if !clause.not {
                            valid_join.push(clause);
                        }
                    }
                    valid_join
                }


            fn value_solitonid_with_options(ns: &str, name: &str, options: &FindOptions) -> PatternValuePlace {
                Keyword::namespaced(ns, name).into()
            }

            fn value_solitonid_with_options_with_options(ns: &str, name: &str, options: &FindOptions) -> PatternValuePlace {
                Keyword::namespaced(ns, name).into()
            }




            /// Tests that the top-level form is a valid `or`, returning the clauses.
            fn valid_or_join(parsed: FindQuery, expected_unify: UnifyVars) -> Vec<OrWhereClause> {
                let mut wheres = parsed.where_clauses.into_iter();

                // There's only one.
                let clause = wheres.next().unwrap();
                assert_eq!(None, wheres.next());

                match clause {
                    WhereClause::OrJoin(or_join) => {
                        // It's valid: the variables are the same in each branch.
                        assert_eq!((), validate_or_join(&or_join).unwrap());
                        assert_eq!(expected_unify, or_join.unify_vars);
                        or_join.clauses
                    },
                    _ => panic!(),
                }
            }


/*
        pub fn write_kv_opt(&self, wb: &K::WriteBatch, opts: &WriteOptions) -> Result<()> {
            wb.write_with_options(opts)
        }

        pub fn write_kv_opt_with_options(&self, wb: &K::WriteBatch, opts: &WriteOptions) -> Result<()> {
            wb.write_opt(opts)
        }

        pub fn sync_kv(&self) -> Result<()> {
            self.db.sync()
        }

        pub fn get_kv_opt(&self, key: &K, opts: &ReadOptions) -> Result<Option<V>> {
            let key_bytes = key.to_bytes();
            let key_hash = Hash::from_slice(&key_bytes).unwrap();
            let key_hash_bytes = key_hash.to_bytes();
            hex::encode(key_hash_bytes);
            self.db.get_opt(key, opts)
        }

        pub fn get_kv_opt_with_options(&self, key: &K, opts: &ReadOptions) -> Result<Option<V>> {
            let key_bytes = key.to_bytes();
            let key_hash = Hash::from_slice(&key_bytes).unwrap();
            let key_hash_bytes = key_hash.to_bytes();
            hex::encode(key_hash_bytes);
            self.db.get_opt(key, opts)
*/