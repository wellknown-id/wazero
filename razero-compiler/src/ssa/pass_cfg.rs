use crate::ssa::basic_block::BasicBlockId;
use crate::ssa::builder::Builder;

#[derive(Clone, Debug, Default)]
pub(crate) struct DominatorSparseTree {
    time: usize,
    euler: Vec<BasicBlockId>,
    first: Vec<i32>,
    depth: Vec<i32>,
    table: Vec<Vec<usize>>,
}

impl DominatorSparseTree {
    pub(crate) fn clear(&mut self) {
        self.time = 0;
        self.euler.clear();
        self.first.clear();
        self.depth.clear();
        self.table.clear();
    }

    fn euler_tour(&mut self, builder: &Builder, node: BasicBlockId, height: i32) {
        self.euler[self.time] = node;
        self.depth[self.time] = height;
        if self.first[node.0 as usize] == -1 {
            self.first[node.0 as usize] = self.time as i32;
        }
        self.time += 1;

        let mut child = builder.block(node).child;
        while let Some(child_id) = child {
            self.euler_tour(builder, child_id, height + 1);
            self.euler[self.time] = node;
            self.depth[self.time] = height;
            self.time += 1;
            child = builder.block(child_id).sibling;
        }
    }

    fn build_sparse_table(&mut self) {
        let n = self.depth.len();
        if n == 0 {
            self.table.clear();
            return;
        }
        let k = floor_log2(n) + 1;
        self.table = vec![vec![0; k]; n];
        for i in 0..n {
            self.table[i][0] = i;
        }
        let mut j = 1;
        while (1usize << j) <= n {
            let width = 1usize << j;
            let half = 1usize << (j - 1);
            for i in 0..=(n - width) {
                let lhs = self.table[i][j - 1];
                let rhs = self.table[i + half][j - 1];
                self.table[i][j] = if self.depth[lhs] < self.depth[rhs] {
                    lhs
                } else {
                    rhs
                };
            }
            j += 1;
        }
    }

    fn rmq(&self, left: usize, right: usize) -> usize {
        let len = right - left + 1;
        let j = floor_log2(len);
        let lhs = self.table[left][j];
        let rhs = self.table[right + 1 - (1usize << j)][j];
        if self.depth[lhs] <= self.depth[rhs] {
            lhs
        } else {
            rhs
        }
    }

    pub(crate) fn find_lca(&self, lhs: BasicBlockId, rhs: BasicBlockId) -> Option<BasicBlockId> {
        let mut left = *self.first.get(lhs.0 as usize)?;
        let mut right = *self.first.get(rhs.0 as usize)?;
        if left < 0 || right < 0 {
            return None;
        }
        if left > right {
            std::mem::swap(&mut left, &mut right);
        }
        Some(self.euler[self.rmq(left as usize, right as usize)])
    }
}

const fn floor_log2(n: usize) -> usize {
    (usize::BITS as usize - 1) - n.leading_zeros() as usize
}

pub(crate) fn pass_calculate_immediate_dominators(builder: &mut Builder) {
    let entry = builder
        .entry_block()
        .expect("entry block must be allocated before running CFG passes");
    for index in 0..builder.block_id_max().0 {
        let block = BasicBlockId(index);
        let block_data = builder.block_mut(block);
        block_data.visited = 0;
        block_data.reverse_post_order = -1;
        block_data.loop_header = false;
    }

    let mut reverse_post_order = Vec::new();
    let mut stack = vec![entry];
    const VISIT_STATE_UNSEEN: i32 = 0;
    const VISIT_STATE_SEEN: i32 = 1;
    const VISIT_STATE_DONE: i32 = 2;
    builder.block_mut(entry).visited = VISIT_STATE_SEEN;

    while let Some(block) = stack.pop() {
        match builder.block(block).visited {
            VISIT_STATE_UNSEEN => panic!("BUG: unsupported CFG"),
            VISIT_STATE_SEEN => {
                stack.push(block);
                let succs = builder.block(block).succs.clone();
                for succ in succs {
                    if succ.is_return() || builder.block(succ).invalid {
                        continue;
                    }
                    if builder.block(succ).visited == VISIT_STATE_UNSEEN {
                        builder.block_mut(succ).visited = VISIT_STATE_SEEN;
                        stack.push(succ);
                    }
                }
                builder.block_mut(block).visited = VISIT_STATE_DONE;
            }
            VISIT_STATE_DONE => reverse_post_order.push(block),
            _ => panic!("BUG"),
        }
    }

    reverse_post_order.reverse();
    for (index, block) in reverse_post_order.iter().copied().enumerate() {
        builder.block_mut(block).reverse_post_order = index as i32;
    }

    builder
        .dominators
        .resize(builder.block_id_max().0 as usize, None);
    calculate_dominators(builder, &reverse_post_order);
    builder.reverse_post_ordered_blocks = reverse_post_order;
    sub_pass_loop_detection(builder);
}

fn calculate_dominators(builder: &mut Builder, reverse_post_ordered_blocks: &[BasicBlockId]) {
    let (&entry, blocks) = reverse_post_ordered_blocks
        .split_first()
        .expect("dominators require a reachable entry block");
    for &block in blocks {
        builder.dominators[block.0 as usize] = None;
    }
    builder.dominators[entry.0 as usize] = Some(entry);

    let mut changed = true;
    while changed {
        changed = false;
        for &block in blocks {
            let preds = builder.block(block).preds.clone();
            let mut intersected = None;
            for pred in preds {
                let pred_block = pred.block;
                if pred_block.is_return()
                    || pred_block.0 as usize >= builder.dominators.len()
                    || builder.dominators[pred_block.0 as usize].is_none()
                {
                    continue;
                }
                intersected = Some(match intersected {
                    None => pred_block,
                    Some(existing) => intersect(builder, existing, pred_block),
                });
            }
            if builder.dominators[block.0 as usize] != intersected {
                builder.dominators[block.0 as usize] = intersected;
                changed = true;
            }
        }
    }
}

fn intersect(builder: &Builder, lhs: BasicBlockId, rhs: BasicBlockId) -> BasicBlockId {
    let mut left = lhs;
    let mut right = rhs;
    while left != right {
        while builder.block(left).reverse_post_order > builder.block(right).reverse_post_order {
            left = builder.dominators[left.0 as usize].expect("dominator must exist");
        }
        while builder.block(right).reverse_post_order > builder.block(left).reverse_post_order {
            right = builder.dominators[right.0 as usize].expect("dominator must exist");
        }
    }
    left
}

fn sub_pass_loop_detection(builder: &mut Builder) {
    for index in 0..builder.block_id_max().0 {
        let block = BasicBlockId(index);
        if !builder.block(block).valid() {
            continue;
        }
        let preds = builder.block(block).preds.clone();
        let mut loop_header = false;
        for pred in preds {
            if pred.block.is_return() || builder.block(pred.block).invalid {
                continue;
            }
            if is_dominated_by(builder, pred.block, block) {
                loop_header = true;
                break;
            }
        }
        builder.block_mut(block).loop_header = loop_header;
    }
}

fn is_dominated_by(builder: &Builder, block: BasicBlockId, dominator: BasicBlockId) -> bool {
    let mut current = Some(block);
    while let Some(node) = current {
        if node == dominator {
            return true;
        }
        let idom = builder.dominators[node.0 as usize];
        if idom == Some(node) {
            break;
        }
        current = idom;
    }
    false
}

pub(crate) fn pass_build_loop_nesting_forest(builder: &mut Builder) {
    builder.loop_nesting_forest_roots.clear();
    for index in 0..builder.block_id_max().0 {
        builder
            .block_mut(BasicBlockId(index))
            .loop_nesting_forest_children
            .clear();
    }
    let entry = builder
        .entry_block()
        .expect("entry block must exist before loop forest construction");
    let blocks = builder.reverse_post_ordered_blocks.clone();
    for block in blocks {
        let mut node = builder.dominators[block.0 as usize].expect("dominator must exist");
        while !builder.block(node).loop_header && node != entry {
            node = builder.dominators[node.0 as usize].expect("dominator must exist");
        }

        if node == entry && builder.block(block).loop_header {
            builder.loop_nesting_forest_roots.push(block);
        } else if node != entry && builder.block(node).loop_header {
            builder
                .block_mut(node)
                .loop_nesting_forest_children
                .push(block);
        }
    }
}

pub(crate) fn pass_build_dominator_tree(builder: &mut Builder) {
    for index in 0..builder.block_id_max().0 {
        let block = builder.block_mut(BasicBlockId(index));
        block.child = None;
        block.sibling = None;
    }

    let blocks = builder.reverse_post_ordered_blocks.clone();
    for block in blocks {
        let parent = builder.dominators[block.0 as usize].expect("dominator must exist");
        if parent == block {
            continue;
        }
        let prev_child = builder.block(parent).child;
        {
            let parent_block = builder.block_mut(parent);
            parent_block.child = Some(block);
        }
        builder.block_mut(block).sibling = prev_child;
    }

    let block_count = builder.block_id_max().0 as usize;
    if block_count == 0 {
        builder.sparse_tree.clear();
        return;
    }

    let entry = builder.entry_block().expect("entry block must exist");
    let mut sparse_tree = DominatorSparseTree {
        time: 0,
        euler: vec![BasicBlockId(0); 2 * block_count - 1],
        first: vec![-1; block_count],
        depth: vec![0; 2 * block_count - 1],
        table: Vec::new(),
    };
    sparse_tree.euler_tour(builder, entry, 0);
    sparse_tree.build_sparse_table();
    builder.sparse_tree = sparse_tree;
}

impl Builder {
    pub fn lowest_common_ancestor(
        &self,
        lhs: BasicBlockId,
        rhs: BasicBlockId,
    ) -> Option<BasicBlockId> {
        self.sparse_tree.find_lca(lhs, rhs)
    }
}

#[cfg(test)]
mod tests {
    use crate::ssa::basic_block::{BasicBlockId, BasicBlockPredecessorInfo};
    use crate::ssa::builder::Builder;
    use crate::ssa::instructions::InstructionId;
    use crate::ssa::signature::{Signature, SignatureId};

    use super::{
        pass_build_dominator_tree, pass_build_loop_nesting_forest,
        pass_calculate_immediate_dominators,
    };

    fn construct_graph_from_edges(edges: &[(u32, &[u32])]) -> Builder {
        let mut builder = Builder::new();
        builder.init(Signature::new(SignatureId(0), vec![], vec![]));
        let max_id = edges
            .iter()
            .flat_map(|(from, tos)| std::iter::once(*from).chain(tos.iter().copied()))
            .max()
            .unwrap_or(0);
        for _ in 0..=max_id {
            builder.allocate_basic_block();
        }
        for (from, tos) in edges {
            for &to in *tos {
                builder
                    .block_mut(BasicBlockId(*from))
                    .succs
                    .push(BasicBlockId(to));
                builder
                    .block_mut(BasicBlockId(to))
                    .preds
                    .push(BasicBlockPredecessorInfo {
                        block: BasicBlockId(*from),
                        branch: InstructionId(0),
                    });
            }
        }
        builder
    }

    #[test]
    fn immediate_dominators_and_loop_headers_match_go_cases() {
        let mut builder = construct_graph_from_edges(&[(0, &[1]), (1, &[2]), (2, &[3]), (3, &[1])]);
        pass_calculate_immediate_dominators(&mut builder);
        assert_eq!(builder.idom(BasicBlockId(1)), Some(BasicBlockId(0)));
        assert_eq!(builder.idom(BasicBlockId(2)), Some(BasicBlockId(1)));
        assert_eq!(builder.idom(BasicBlockId(3)), Some(BasicBlockId(2)));
        assert!(builder.block(BasicBlockId(1)).loop_header);
        assert!(!builder.block(BasicBlockId(2)).loop_header);
    }

    #[test]
    fn loop_nesting_forest_tracks_nested_loop_headers() {
        let mut builder =
            construct_graph_from_edges(&[(0, &[1]), (1, &[2]), (2, &[1, 3]), (3, &[2, 4])]);
        pass_calculate_immediate_dominators(&mut builder);
        pass_build_loop_nesting_forest(&mut builder);
        assert_eq!(builder.loop_nesting_forest_roots(), &[BasicBlockId(1)]);
        assert_eq!(
            builder.block(BasicBlockId(1)).loop_nesting_forest_children,
            vec![BasicBlockId(2)]
        );
        assert_eq!(
            builder.block(BasicBlockId(2)).loop_nesting_forest_children,
            vec![BasicBlockId(3), BasicBlockId(4)]
        );
    }

    #[test]
    fn dominator_sparse_tree_finds_lca() {
        let mut builder = construct_graph_from_edges(&[(0, &[1, 2]), (1, &[3, 4]), (2, &[5, 6])]);
        pass_calculate_immediate_dominators(&mut builder);
        pass_build_dominator_tree(&mut builder);
        assert_eq!(
            builder.lowest_common_ancestor(BasicBlockId(3), BasicBlockId(4)),
            Some(BasicBlockId(1))
        );
        assert_eq!(
            builder.lowest_common_ancestor(BasicBlockId(3), BasicBlockId(5)),
            Some(BasicBlockId(0))
        );
        assert_eq!(
            builder.lowest_common_ancestor(BasicBlockId(5), BasicBlockId(6)),
            Some(BasicBlockId(2))
        );
    }
}
