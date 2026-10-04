// Adapted from matchit's excellent Radix Tree implementation
use std::{
    cmp::{Reverse, min},
    mem,
    ops::Range,
    str::Utf8Error,
};

use super::{RouteInfo, params::Param, pattern::Token};

#[derive(Clone, Copy)]
pub(super) struct Candidate {
    pub(super) id: usize,
    pub(super) implicit: bool,
}

pub(super) enum InsertError {
    Conflict(usize),
    Duplicate(usize),
}

#[derive(Default)]
pub(super) struct MatcherBuilder {
    root: BuildNode,
}

#[derive(Default)]
struct BuildNode {
    prefix: Vec<Token>,
    priority: usize,
    wild_child: bool,
    indices: Vec<Token>,
    node_type: NodeType,
    children: Vec<BuildNode>,
    candidates: Vec<Candidate>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum NodeType {
    Root,
    #[default]
    Static,
    Param {
        suffix: bool,
    },
    CatchAll,
}

pub(super) struct Matcher {
    root: Node,
}

// Immutable nodes retain only lookup data. Static prefixes and child indexes
// are literal bytes; construction tokens, priorities, and suffix flags are gone.
struct Node {
    prefix: Box<[u8]>,
    indices: Box<[u8]>,
    children: Box<[Node]>,
    candidates: Box<[Candidate]>,
    kind: NodeKind,
    wild_child: bool,
}

#[derive(Clone, Copy)]
enum NodeKind {
    Static,
    Param,
    CatchAll,
}

pub(super) struct Match {
    pub(super) id: usize,
    pub(super) params: Vec<Param>,
}

impl MatcherBuilder {
    pub(super) fn insert(
        &mut self,
        tokens: &[Token],
        candidate: Candidate,
        routes: &[RouteInfo],
    ) -> Result<(), InsertError> {
        self.root.insert_tokens(tokens, candidate, routes)
    }

    pub(super) fn finish(self, routes: &[RouteInfo]) -> Matcher {
        Matcher {
            root: self.root.freeze(routes),
        }
    }
}

impl BuildNode {
    fn freeze(mut self, routes: &[RouteInfo]) -> Node {
        let kind = match self.node_type {
            NodeType::Root | NodeType::Static => NodeKind::Static,
            NodeType::Param { .. } => NodeKind::Param,
            NodeType::CatchAll => NodeKind::CatchAll,
        };
        let prefix = match kind {
            NodeKind::Static => literal_bytes(self.prefix),
            NodeKind::Param | NodeKind::CatchAll => Box::default(),
        };
        self.candidates.sort_unstable_by_key(|candidate| {
            (
                candidate.implicit,
                Reverse(routes[candidate.id].pattern.constraint_count()),
                candidate.id,
            )
        });
        Node {
            prefix,
            indices: literal_bytes(self.indices),
            children: self.children.into_iter().map(|child| child.freeze(routes)).collect(),
            candidates: self.candidates.into_boxed_slice(),
            kind,
            wild_child: self.wild_child,
        }
    }
}

fn literal_bytes(tokens: Vec<Token>) -> Box<[u8]> {
    tokens
        .into_iter()
        .map(|token| match token {
            Token::Byte(byte) => byte,
            _ => unreachable!("static prefixes and child indexes contain only literals"),
        })
        .collect()
}

impl Matcher {
    pub(super) fn at(&self, path: &str, routes: &[RouteInfo]) -> Result<Option<Match>, Utf8Error> {
        let mut params = Vec::new();
        Ok(self.search(path, routes, &mut params)?.map(|id| Match { id, params }))
    }

    // Probes retain decoding and constraint semantics without materializing
    // request metadata or a Match. Scratch values are discarded after lookup.
    pub(super) fn matches(&self, path: &str, routes: &[RouteInfo]) -> Result<bool, Utf8Error> {
        Ok(self.search(path, routes, &mut Vec::new())?.is_some())
    }

    fn search(&self, path: &str, routes: &[RouteInfo], params: &mut Vec<Param>) -> Result<Option<usize>, Utf8Error> {
        let mut stack = Vec::new();
        let mut next = Some(Search {
            node: &self.root,
            offset: 0,
            params: 0,
            capture: None,
            leaf: false,
        });
        let mut ranges = Vec::new();
        while let Some(search) = next.take().or_else(|| stack.pop()) {
            ranges.truncate(search.params);
            if let Some(capture) = search.capture {
                ranges.push(capture);
            }
            let node = search.node;
            let offset = search.offset;
            if search.leaf {
                if let Some(id) = node.qualify(path, &ranges, routes, params)? {
                    return Ok(Some(id));
                }
                continue;
            }
            match node.kind {
                NodeKind::Static => {
                    let Some(prefix) = path.as_bytes().get(offset..offset + node.prefix.len()) else {
                        continue;
                    };
                    if prefix != &*node.prefix {
                        continue;
                    }
                    let offset = offset + node.prefix.len();
                    if offset == path.len() {
                        if let Some(id) = node.qualify(path, &ranges, routes, params)? {
                            return Ok(Some(id));
                        }
                        continue;
                    }
                    // Push the wildcard first so a matching static child is visited first.
                    if node.wild_child {
                        Search {
                            node: node.children.last().expect("wildcards are stored last"),
                            offset,
                            params: ranges.len(),
                            capture: None,
                            leaf: false,
                        }
                        .schedule(&mut next, &mut stack);
                    }
                    if let Some(i) = node.indices.iter().position(|&byte| byte == path.as_bytes()[offset]) {
                        Search {
                            node: &node.children[i],
                            offset,
                            params: ranges.len(),
                            capture: None,
                            leaf: false,
                        }
                        .schedule(&mut next, &mut stack);
                    }
                }
                NodeKind::Param => {
                    let slash = path.as_bytes()[offset..].iter().position(|&byte| byte == b'/');
                    let end = slash.map_or(path.len(), |i| offset + i);
                    if end == offset {
                        continue;
                    }
                    // A plain capture is the last choice after all matching suffixes.
                    if slash.is_none() && !node.candidates.is_empty() {
                        Search {
                            node,
                            offset: end,
                            params: ranges.len(),
                            capture: Some(offset..end),
                            leaf: true,
                        }
                        .schedule(&mut next, &mut stack);
                    }
                    let terminator = end + usize::from(slash.is_some());
                    for child in node.children.iter().rev() {
                        if child.prefix.len() >= terminator - offset {
                            continue;
                        }
                        let start = terminator - child.prefix.len();
                        if path.as_bytes()[start..terminator] == *child.prefix {
                            Search {
                                node: child,
                                offset: start,
                                params: ranges.len(),
                                capture: Some(offset..start),
                                leaf: false,
                            }
                            .schedule(&mut next, &mut stack);
                        }
                    }
                }
                NodeKind::CatchAll => {
                    if offset == path.len() {
                        continue;
                    }
                    ranges.push(offset..path.len());
                    if let Some(id) = node.qualify(path, &ranges, routes, params)? {
                        return Ok(Some(id));
                    }
                }
            }
        }
        Ok(None)
    }
}

impl Node {
    fn qualify(
        &self,
        path: &str,
        ranges: &[Range<usize>],
        routes: &[RouteInfo],
        params: &mut Vec<Param>,
    ) -> Result<Option<usize>, Utf8Error> {
        if self.candidates.is_empty() {
            return Ok(None);
        }
        params.clear();
        for range in ranges {
            params.push(Param::decode(path, range.clone())?);
        }
        Ok(self
            .candidates
            .iter()
            .find(|candidate| {
                routes[candidate.id]
                    .pattern
                    .captures
                    .iter()
                    .zip(params.iter())
                    .all(|(capture, value)| capture.matches(value.value(path)))
            })
            .map(|candidate| candidate.id))
    }
}

struct Search<'a> {
    node: &'a Node,
    offset: usize,
    params: usize,
    capture: Option<Range<usize>>,
    leaf: bool,
}

impl<'a> Search<'a> {
    // Keep the next preferred branch inline. Allocate a stack only when another
    // branch must be saved for backtracking.
    fn schedule(self, next: &mut Option<Self>, stack: &mut Vec<Self>) {
        if let Some(prior) = next.replace(self) {
            stack.push(prior);
        }
    }
}

fn boundary(tokens: &[Token]) -> bool {
    tokens.is_empty() || tokens == [Token::Byte(b'/')]
}

fn find_wildcard(tokens: &[Token]) -> Option<Range<usize>> {
    tokens.iter().position(|token| token.is_capture()).map(|i| i..i + 1)
}

struct InsertState<'node> {
    parent: &'node mut BuildNode,
    child: Option<usize>,
}

impl<'node> InsertState<'node> {
    fn parent(&self) -> Option<&BuildNode> {
        match self.child {
            None => None,
            Some(_) => Some(self.parent),
        }
    }

    fn node(&self) -> &BuildNode {
        match self.child {
            None => self.parent,
            Some(i) => &self.parent.children[i],
        }
    }

    fn node_mut(&mut self) -> &mut BuildNode {
        match self.child {
            None => self.parent,
            Some(i) => &mut self.parent.children[i],
        }
    }

    fn set_child(self, i: usize) -> InsertState<'node> {
        match self.child {
            None => InsertState {
                parent: self.parent,
                child: Some(i),
            },
            Some(prev) => InsertState {
                parent: &mut self.parent.children[prev],
                child: Some(i),
            },
        }
    }
}

impl BuildNode {
    fn store(&mut self, candidate: Candidate, routes: &[RouteInfo]) -> Result<(), InsertError> {
        if let Some(prior) = self.candidates.iter().find(|prior| {
            prior.implicit == candidate.implicit
                && routes[prior.id].pattern.same_constraints(&routes[candidate.id].pattern)
        }) {
            return Err(InsertError::Duplicate(prior.id));
        }
        self.candidates.push(candidate);
        Ok(())
    }

    fn first_id(&self) -> usize {
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            if let Some(candidate) = node.candidates.first() {
                return candidate.id;
            }
            stack.extend(&node.children);
        }
        unreachable!("structural conflicts refer to an existing endpoint")
    }

    fn insert_tokens(&mut self, route: &[Token], val: Candidate, routes: &[RouteInfo]) -> Result<(), InsertError> {
        let mut remaining = route;

        self.priority += 1;

        if self.candidates.is_empty() && self.children.is_empty() {
            self.insert_route(remaining, val);
            self.node_type = NodeType::Root;
            return Ok(());
        }

        let mut state = InsertState {
            parent: self,
            child: None,
        };

        'walk: loop {
            let len = min(remaining.len(), state.node().prefix.len());
            let common_prefix = (0..len)
                .find(|&i| remaining[i] != state.node().prefix[i])
                .unwrap_or(len);

            if state.node().prefix.len() > common_prefix {
                let node = state.node_mut();

                let suffix = node.prefix[common_prefix..].to_owned();
                let child = BuildNode {
                    prefix: suffix,
                    candidates: mem::take(&mut node.candidates),
                    indices: mem::take(&mut node.indices),
                    wild_child: node.wild_child,
                    children: mem::take(&mut node.children),
                    priority: node.priority - 1,
                    node_type: NodeType::Static,
                };

                node.children = vec![child];
                node.indices = vec![node.prefix[common_prefix]];
                node.prefix = node.prefix[..common_prefix].to_owned();
                node.wild_child = false;
                continue;
            }

            if remaining.len() == common_prefix {
                let node = state.node_mut();

                node.store(val, routes)?;
                return Ok(());
            }

            let common_remaining = remaining;
            remaining = &remaining[common_prefix..];
            let next = remaining[0];

            if let NodeType::Param { suffix: has_suffix } = state.node().node_type {
                let terminator = remaining
                    .iter()
                    .position(|&b| b == Token::Byte(b'/'))
                    .map(|b| b + 1)
                    .unwrap_or(remaining.len());

                let suffix = &remaining[..terminator];

                let mut extra_trailing_slash = false;
                for (i, child) in state.node().children.iter().enumerate() {
                    if *child.prefix == *suffix {
                        state = state.set_child(i);
                        state.node_mut().priority += 1;
                        continue 'walk;
                    }

                    if child.prefix.len() <= suffix.len() {
                        let (common, remaining) = suffix.split_at(child.prefix.len());
                        if *common == *child.prefix && remaining == [Token::Byte(b'/')] {
                            extra_trailing_slash = true;
                        }
                    } else {
                        let (common, remaining) = child.prefix.split_at(suffix.len());
                        if *common == *suffix && remaining == [Token::Byte(b'/')] {
                            extra_trailing_slash = true;
                        }
                    }
                }

                if !extra_trailing_slash
                    && !boundary(suffix)
                    && let Some(parent) = state.parent()
                    && let Some(id) = parent.prefix_parameter_id()
                {
                    return Err(InsertError::Conflict(id));
                }

                if find_wildcard(suffix).is_some() {
                    unreachable!("patterns allow only one capture per segment");
                }

                let child = state.node_mut().add_suffix_child(BuildNode {
                    prefix: suffix.to_owned(),
                    node_type: NodeType::Static,
                    priority: 1,
                    ..BuildNode::default()
                });

                let has_suffix = has_suffix || !boundary(suffix);
                state.node_mut().node_type = NodeType::Param { suffix: has_suffix };

                state = state.set_child(child);

                if terminator == remaining.len() {
                    state.node_mut().candidates.push(val);
                    return Ok(());
                }

                remaining = &remaining[terminator..];

                if !remaining[0].is_capture() {
                    let child = state.node_mut().add_child(BuildNode {
                        node_type: NodeType::Static,
                        priority: 1,
                        ..BuildNode::default()
                    });
                    state.node_mut().indices.push(remaining[0]);
                    state = state.set_child(child);
                }

                state.node_mut().insert_route(remaining, val);
                return Ok(());
            }

            for mut i in 0..state.node().indices.len() {
                if next == state.node().indices[i] {
                    i = state.node_mut().update_child_priority(i);
                    state = state.set_child(i);
                    continue 'walk;
                }
            }

            if !next.is_capture() && state.node().node_type != NodeType::CatchAll {
                let node = state.node_mut();

                let terminator = remaining
                    .iter()
                    .position(|&b| b == Token::Byte(b'/'))
                    .unwrap_or(remaining.len());

                if let Some(wildcard) = find_wildcard(&remaining[..terminator]) {
                    if wildcard.start > 0
                        && let Some(id) = node.suffix_parameter_id()
                    {
                        return Err(InsertError::Conflict(id));
                    }

                    let suffix = &remaining[wildcard.end..terminator];
                    if !boundary(suffix)
                        && let Some(id) = node.prefix_parameter_id()
                    {
                        return Err(InsertError::Conflict(id));
                    }
                }

                node.indices.push(next);
                let child = node.add_child(BuildNode::default());
                let child = node.update_child_priority(child);

                node.children[child].insert_route(remaining, val);
                return Ok(());
            }

            if state.node().wild_child {
                let wild_child = state.node().children.len() - 1;
                state = state.set_child(wild_child);
                state.node_mut().priority += 1;

                if let Some(wildcard) = remaining.get(..state.node().prefix.len())
                    && wildcard != state.node().prefix
                {
                    return Err(InsertError::Conflict(state.node().first_id()));
                }

                if state.node().node_type == NodeType::CatchAll {
                    if remaining == state.node().prefix {
                        return state.node_mut().store(val, routes);
                    }
                    return Err(InsertError::Conflict(state.node().first_id()));
                }

                if let Some(parent) = state.parent()
                    && !parent.prefix.ends_with(&[Token::Byte(b'/')])
                    && matches!(state.node().node_type, NodeType::Param { suffix: true })
                {
                    let terminator = remaining
                        .iter()
                        .position(|&b| b == Token::Byte(b'/'))
                        .map(|b| b + 1)
                        .unwrap_or(remaining.len());

                    if let Some(wildcard) = find_wildcard(&remaining[..terminator]) {
                        let suffix = &remaining[wildcard.end..];
                        if boundary(suffix) {
                            return Err(InsertError::Conflict(
                                state.node().suffix_parameter_id().expect("parameter has a suffix"),
                            ));
                        }
                    }
                }

                continue 'walk;
            }

            if let Some(wildcard) = find_wildcard(remaining) {
                let node = state.node();
                let suffix = &remaining[wildcard.end..];

                if !boundary(suffix)
                    && let Some(id) = node.prefix_parameter_id()
                {
                    return Err(InsertError::Conflict(id));
                }

                if let Some(i) = common_prefix.checked_sub(1)
                    && common_remaining[i] != Token::Byte(b'/')
                    && let Some(id) = node.suffix_parameter_id()
                {
                    return Err(InsertError::Conflict(id));
                }
            }

            state.node_mut().insert_route(remaining, val);
            return Ok(());
        }
    }

    // Keep matchit's prefix/suffix ambiguity rules and identify the actual
    // parameter endpoint responsible, rather than replaying insertions.
    fn prefix_parameter_id(&self) -> Option<usize> {
        if self.node_type == NodeType::Root && self.prefix.is_empty() {
            return None;
        }
        if self.prefix.ends_with(&[Token::Byte(b'/')]) {
            self.children.iter().find_map(BuildNode::prefix_parameter_id)
        } else {
            self.children.iter().find_map(BuildNode::parameter_id)
        }
    }

    fn parameter_id(&self) -> Option<usize> {
        if self.prefix.contains(&Token::Byte(b'/')) {
            return None;
        }
        if matches!(self.node_type, NodeType::Param { .. }) {
            return Some(self.first_id());
        }
        self.children.iter().find_map(BuildNode::parameter_id)
    }

    fn suffix_parameter_id(&self) -> Option<usize> {
        if self.node_type == (NodeType::Param { suffix: true }) {
            return self
                .children
                .iter()
                .find(|child| !boundary(&child.prefix))
                .map(BuildNode::first_id);
        }
        self.children
            .iter()
            .filter(|child| !child.prefix.contains(&Token::Byte(b'/')))
            .find_map(BuildNode::suffix_parameter_id)
    }

    // Extending a new branch cannot conflict; Pattern has already validated syntax.
    fn insert_route(&mut self, mut prefix: &[Token], val: Candidate) {
        let mut node = self;

        loop {
            let Some(wildcard) = find_wildcard(prefix) else {
                node.candidates.push(val);
                node.prefix = prefix.to_owned();
                return;
            };

            if prefix[wildcard.start] == Token::CatchAll {
                if wildcard.end != prefix.len() {
                    unreachable!("patterns validate terminal catch-alls");
                }

                if wildcard.start > 0 {
                    node.prefix = prefix[..wildcard.start].to_owned();
                    prefix = &prefix[wildcard.start..];
                }

                node.add_child(BuildNode {
                    prefix: prefix.to_owned(),
                    node_type: NodeType::CatchAll,
                    candidates: vec![val],
                    priority: 1,
                    ..BuildNode::default()
                });
                node.wild_child = true;
                return;
            }

            if wildcard.start > 0 {
                node.prefix = prefix[..wildcard.start].to_owned();
                prefix = &prefix[wildcard.start..];
            }

            let terminator = prefix
                .iter()
                .position(|&b| b == Token::Byte(b'/'))
                .map(|b| b + 1)
                .unwrap_or(prefix.len());

            let wildcard = &prefix[..wildcard.len()];
            let suffix = &prefix[wildcard.len()..terminator];
            prefix = &prefix[terminator..];

            if find_wildcard(suffix).is_some() {
                unreachable!("patterns allow only one capture per segment");
            }

            let has_suffix = !boundary(suffix);
            let child = node.add_child(BuildNode {
                priority: 1,
                node_type: NodeType::Param { suffix: has_suffix },
                prefix: wildcard.to_owned(),
                ..BuildNode::default()
            });

            node.wild_child = true;
            node = &mut node.children[child];

            if !suffix.is_empty() {
                let child = node.add_suffix_child(BuildNode {
                    priority: 1,
                    node_type: NodeType::Static,
                    prefix: suffix.to_owned(),
                    ..BuildNode::default()
                });

                node = &mut node.children[child];
            }

            if prefix.is_empty() {
                node.candidates.push(val);
                return;
            }

            if !prefix[0].is_capture() {
                node.indices.push(prefix[0]);
                let child = node.add_child(BuildNode {
                    priority: 1,
                    ..BuildNode::default()
                });
                node = &mut node.children[child];
            }
        }
    }

    fn add_child(&mut self, child: BuildNode) -> usize {
        let len = self.children.len();

        if self.wild_child && len > 0 {
            self.children.insert(len - 1, child);
            len - 1
        } else {
            self.children.push(child);
            len
        }
    }

    fn add_suffix_child(&mut self, child: BuildNode) -> usize {
        let i = self
            .children
            .partition_point(|node| node.prefix.len() >= child.prefix.len());
        self.children.insert(i, child);
        i
    }

    fn update_child_priority(&mut self, i: usize) -> usize {
        self.children[i].priority += 1;
        let priority = self.children[i].priority;

        let mut updated = i;
        while updated > 0 && self.children[updated - 1].priority < priority {
            self.children.swap(updated - 1, updated);
            updated -= 1;
        }

        if updated != i {
            self.indices[updated..=i].rotate_right(1);
        }

        updated
    }
}
