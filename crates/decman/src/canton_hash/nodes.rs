//! Transaction-node encoding for the interactive-submission hashing scheme.
//!
//! Port of Canton's `NodeHashBuilderCommon` plus the `v2` / `v3` overrides
//! (`community/base/.../protocol/hash/`), cross-checked against the reference
//! Python implementations Canton ships with the interactive-submission example.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
};

use canton_proto_rs::com::daml::ledger::api::v2::interactive::{
    DamlTransaction, GlobalKeyWithMaintainers,
    daml_transaction::{NodeSeed, node::VersionedNode},
    transaction::v1::{Create, Exercise, Fetch, Node, QueryByKey, Rollback, node::NodeType},
};

use crate::{
    canton_hash::{
        HashingScheme,
        encoding::{Encoder, required, sha256},
    },
    error::Result,
};

/// Node tags. A node's kind is part of its encoding so two different node
/// kinds can never hash alike.
const CREATE_TAG: u8 = 0x00;
const EXERCISE_TAG: u8 = 0x01;
const FETCH_TAG: u8 = 0x02;
const ROLLBACK_TAG: u8 = 0x03;
const QUERY_BY_KEY_TAG: u8 = 0x04;

/// V2 prefixes every node with the version of the protobuf used to encode it.
/// V3 dropped the prefix (it carries no security value — see the comment on
/// `NodeEncodingV1` in Canton's `v2/NodeHashBuilder.scala`).
const NODE_ENCODING_V1: u8 = 0x01;

/// Depth limit on the node tree. The transaction comes from a coordinator we
/// are explicitly not trusting, so a pathological tree must fail rather than
/// exhaust the stack. Canton's own transactions are far shallower.
const MAX_NODE_DEPTH: usize = 100;

/// Resolves node references while hashing a transaction tree.
pub(super) struct NodeHasher<'a> {
    scheme: HashingScheme,
    nodes: HashMap<&'a str, &'a Node>,
    seeds: HashMap<String, &'a [u8]>,
    /// Completed node hashes, keyed by node id.
    ///
    /// A node's hash depends only on the node and the subtree below it, so it
    /// is the same every time it is referenced. Without this, a transaction
    /// whose nodes each name the same child twice costs 2^depth work — the
    /// depth limit alone caps that at 2^100, which an untrusted coordinator
    /// could use to stall a peer inside hash verification.
    memo: RefCell<HashMap<String, [u8; 32]>>,
}

impl<'a> NodeHasher<'a> {
    /// Index a transaction's nodes and seeds for lookup by node id.
    ///
    /// # Errors
    ///
    /// Errors if two nodes share a node id — the references in `roots` and in
    /// `children` would then be ambiguous, and picking either one would mean
    /// hashing something other than what the sender meant.
    pub(super) fn new(scheme: HashingScheme, transaction: &'a DamlTransaction) -> Result<Self> {
        let mut nodes = HashMap::with_capacity(transaction.nodes.len());
        for node in &transaction.nodes {
            let VersionedNode::V1(inner) = required(
                node.versioned_node.as_ref(),
                "versioned node (only v1 nodes are supported)",
            )?;
            if nodes.insert(node.node_id.as_str(), inner).is_some() {
                anyhow::bail!("duplicate node id {id} in transaction", id = node.node_id);
            }
        }

        let seeds = transaction
            .node_seeds
            .iter()
            .map(|NodeSeed { node_id, seed }| (node_id.to_string(), seed.as_slice()))
            .collect();

        Ok(Self {
            scheme,
            nodes,
            seeds,
            memo: RefCell::new(HashMap::new()),
        })
    }

    /// Hash the node `node_id` refers to. This is how both root nodes and
    /// child nodes enter an enclosing encoding: the node id itself is opaque
    /// and is never hashed.
    pub(super) fn hash_node_id(&self, node_id: &str) -> Result<[u8; 32]> {
        self.hash_node_id_at(node_id, 0, &mut HashSet::new())
    }

    fn hash_node_id_at(
        &self,
        node_id: &str,
        depth: usize,
        path: &mut HashSet<String>,
    ) -> Result<[u8; 32]> {
        if depth > MAX_NODE_DEPTH {
            anyhow::bail!("transaction tree exceeds the {MAX_NODE_DEPTH} level depth limit");
        }
        if let Some(hash) = self.memo.borrow().get(node_id) {
            return Ok(*hash);
        }
        // The path set still guards recursion, so a genuine cycle is reported
        // rather than served from the memo: a node only reaches the memo once
        // its whole subtree has been hashed without revisiting it.
        if !path.insert(node_id.to_string()) {
            anyhow::bail!("transaction tree references node {node_id} cyclically");
        }
        let node = self
            .nodes
            .get(node_id)
            .ok_or_else(|| anyhow::anyhow!("transaction references missing node {node_id}"))?;
        let encoded = self.encode_node(node, node_id, depth, path)?;
        path.remove(node_id);
        let hash = sha256(&encoded);
        self.memo.borrow_mut().insert(node_id.to_string(), hash);
        Ok(hash)
    }

    /// Hash a create node that is not part of the transaction tree — the
    /// input contracts carried in the metadata. Those have no node seed and
    /// no children, so they need no node index.
    pub(super) fn hash_detached_create(scheme: HashingScheme, create: &Create) -> Result<[u8; 32]> {
        let hasher = Self {
            scheme,
            nodes: HashMap::new(),
            seeds: HashMap::new(),
            memo: RefCell::new(HashMap::new()),
        };
        let mut encoder = hasher.new_node_encoder();
        hasher.encode_create(&mut encoder, create, None)?;
        Ok(encoder.digest())
    }

    /// V2 prefixes every node encoding — including the detached create nodes
    /// in the metadata — with the node protobuf's encoding version.
    fn new_node_encoder(&self) -> Encoder {
        let mut encoder = Encoder::new();
        if self.scheme == HashingScheme::V2 {
            encoder.byte(NODE_ENCODING_V1);
        }
        encoder
    }

    fn encode_node(
        &self,
        node: &Node,
        node_id: &str,
        depth: usize,
        path: &mut HashSet<String>,
    ) -> Result<Vec<u8>> {
        let mut encoder = self.new_node_encoder();
        let node_type = required(node.node_type.as_ref(), "node type")?;
        match node_type {
            NodeType::Create(create) => {
                self.encode_create(&mut encoder, create, self.seed(node_id))?;
            }
            NodeType::Fetch(fetch) => self.encode_fetch(&mut encoder, fetch)?,
            NodeType::Exercise(exercise) => {
                self.encode_exercise(&mut encoder, exercise, node_id, depth, path)?;
            }
            NodeType::Rollback(rollback) => {
                self.encode_rollback(&mut encoder, rollback, depth, path)?;
            }
            NodeType::QueryByKey(query) => self.encode_query_by_key(&mut encoder, query)?,
        }
        Ok(encoder.into_bytes())
    }

    fn seed(&self, node_id: &str) -> Option<&'a [u8]> {
        self.seeds.get(node_id).copied()
    }

    fn encode_create(
        &self,
        encoder: &mut Encoder,
        create: &Create,
        seed: Option<&[u8]>,
    ) -> Result<()> {
        encoder.string(&create.lf_version)?;
        encoder.byte(CREATE_TAG);
        match seed {
            Some(seed) => {
                encoder.byte(0x01);
                encoder.add_hash(seed);
            }
            None => encoder.byte(0x00),
        }
        encoder.hex_string(&create.contract_id)?;
        encoder.string(&create.package_name)?;
        encoder.identifier(required(create.template_id.as_ref(), "create template id")?)?;
        encoder.value(required(create.argument.as_ref(), "create argument")?)?;
        encoder.string_set(&create.signatories)?;
        encoder.string_set(&create.stakeholders)?;
        self.encode_optional_key(encoder, create.key.as_ref(), "Create")
    }

    fn encode_fetch(&self, encoder: &mut Encoder, fetch: &Fetch) -> Result<()> {
        encoder.string(&fetch.lf_version)?;
        encoder.byte(FETCH_TAG);
        encoder.hex_string(&fetch.contract_id)?;
        encoder.string(&fetch.package_name)?;
        encoder.identifier(required(fetch.template_id.as_ref(), "fetch template id")?)?;
        encoder.string_set(&fetch.signatories)?;
        encoder.string_set(&fetch.stakeholders)?;
        encoder.optional(fetch.interface_id.as_ref(), Encoder::identifier)?;
        encoder.string_set(&fetch.acting_parties)?;
        self.encode_by_key(encoder, fetch.by_key, "Fetch")?;
        self.encode_optional_key(encoder, fetch.key.as_ref(), "Fetch")
    }

    fn encode_exercise(
        &self,
        encoder: &mut Encoder,
        exercise: &Exercise,
        node_id: &str,
        depth: usize,
        path: &mut HashSet<String>,
    ) -> Result<()> {
        // External call results only exist on development-version nodes and
        // are hashed from V4 on. Refuse rather than silently drop them from
        // the hash: a field we do not hash is a field a coordinator can
        // change without invalidating our signature.
        if !exercise.external_call_results.is_empty() {
            anyhow::bail!(
                "exercise node carries external call results, which hashing scheme \
                 {scheme} does not cover",
                scheme = self.scheme
            );
        }
        let seed = self
            .seed(node_id)
            .ok_or_else(|| anyhow::anyhow!("exercise node {node_id} has no node seed"))?;

        encoder.string(&exercise.lf_version)?;
        encoder.byte(EXERCISE_TAG);
        encoder.add_hash(seed);
        encoder.hex_string(&exercise.contract_id)?;
        encoder.string(&exercise.package_name)?;
        encoder.identifier(required(
            exercise.template_id.as_ref(),
            "exercise template id",
        )?)?;
        encoder.string_set(&exercise.signatories)?;
        encoder.string_set(&exercise.stakeholders)?;
        encoder.string_set(&exercise.acting_parties)?;
        encoder.optional(exercise.interface_id.as_ref(), Encoder::identifier)?;
        encoder.string(&exercise.choice_id)?;
        encoder.value(required(
            exercise.chosen_value.as_ref(),
            "exercise chosen value",
        )?)?;
        encoder.bool(exercise.consuming);
        encoder.optional(exercise.exercise_result.as_ref(), Encoder::value)?;
        encoder.string_set(&exercise.choice_observers)?;
        self.encode_by_key(encoder, exercise.by_key, "Exercise")?;
        self.encode_optional_key(encoder, exercise.key.as_ref(), "Exercise")?;
        self.encode_children(encoder, &exercise.children, depth, path)
    }

    fn encode_rollback(
        &self,
        encoder: &mut Encoder,
        rollback: &Rollback,
        depth: usize,
        path: &mut HashSet<String>,
    ) -> Result<()> {
        encoder.byte(ROLLBACK_TAG);
        self.encode_children(encoder, &rollback.children, depth, path)
    }

    fn encode_query_by_key(&self, encoder: &mut Encoder, query: &QueryByKey) -> Result<()> {
        if self.scheme == HashingScheme::V2 {
            anyhow::bail!("QueryByKey nodes are not supported by hashing scheme V2");
        }
        encoder.string(&query.lf_version)?;
        encoder.byte(QUERY_BY_KEY_TAG);
        encoder.string(&query.package_name)?;
        encoder.identifier(required(query.template_id.as_ref(), "query template id")?)?;
        encoder.bool(query.exhaustive);
        encode_key_with_maintainers(encoder, required(query.key.as_ref(), "query key")?)?;
        encoder.repeated(&query.result, |encoder, contract_id| {
            encoder.hex_string(contract_id)
        })
    }

    fn encode_children(
        &self,
        encoder: &mut Encoder,
        children: &[String],
        depth: usize,
        path: &mut HashSet<String>,
    ) -> Result<()> {
        encoder.repeated(children, |encoder, child| {
            let hash = self.hash_node_id_at(child, depth + 1, path)?;
            encoder.add_hash(&hash);
            Ok(())
        })
    }

    /// Contract keys entered the hash in V3. A V2 payload carrying one would
    /// hash without it, so refuse — exactly as Canton's V2 builder does.
    fn encode_optional_key(
        &self,
        encoder: &mut Encoder,
        key: Option<&GlobalKeyWithMaintainers>,
        node_kind: &str,
    ) -> Result<()> {
        match self.scheme {
            HashingScheme::V2 => {
                if key.is_some() {
                    anyhow::bail!(
                        "{node_kind} node carries a contract key, which hashing scheme V2 \
                         does not cover"
                    );
                }
                Ok(())
            }
            HashingScheme::V3 => encoder.optional(key, encode_key_with_maintainers),
        }
    }

    fn encode_by_key(&self, encoder: &mut Encoder, by_key: bool, node_kind: &str) -> Result<()> {
        match self.scheme {
            HashingScheme::V2 => {
                if by_key {
                    anyhow::bail!(
                        "{node_kind} node is flagged by-key, which hashing scheme V2 does \
                         not cover"
                    );
                }
                Ok(())
            }
            HashingScheme::V3 => {
                encoder.bool(by_key);
                Ok(())
            }
        }
    }
}

fn encode_key_with_maintainers(
    encoder: &mut Encoder,
    key: &GlobalKeyWithMaintainers,
) -> Result<()> {
    let global_key = required(key.key.as_ref(), "global key")?;
    encoder.string(&global_key.package_name)?;
    encoder.identifier(required(
        global_key.template_id.as_ref(),
        "key template id",
    )?)?;
    encoder.value(required(global_key.key.as_ref(), "key value")?)?;
    encoder.add_hash(&global_key.hash);
    encoder.string_set(&key.maintainers)
}

/// Hash the transaction tree: its serialization version followed by the hash
/// of each root node.
pub(super) fn hash_transaction(
    scheme: HashingScheme,
    transaction: &DamlTransaction,
) -> Result<[u8; 32]> {
    let hasher = NodeHasher::new(scheme, transaction)?;
    let mut encoder = Encoder::with_purpose();
    encoder.string(&transaction.version)?;
    encoder.repeated(&transaction.roots, |encoder, root| {
        let hash = hasher.hash_node_id(root)?;
        encoder.add_hash(&hash);
        Ok(())
    })?;
    Ok(encoder.digest())
}
