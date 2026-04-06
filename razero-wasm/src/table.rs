#![doc = "Runtime-side Wasm table instances."]

use crate::module::{RefType, Table};

pub type Reference = Option<u32>;
pub type ElementInstance = Vec<Reference>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInstance {
    pub elements: ElementInstance,
    pub min: u32,
    pub max: Option<u32>,
    pub ty: RefType,
}

impl Default for TableInstance {
    fn default() -> Self {
        Self {
            elements: Vec::new(),
            min: 0,
            max: None,
            ty: RefType::FUNCREF,
        }
    }
}

impl TableInstance {
    pub fn new(table: &Table) -> Self {
        Self {
            elements: vec![None; table.min as usize],
            min: table.min,
            max: table.max,
            ty: table.ty,
        }
    }

    pub fn grow(&mut self, delta: u32, initial_ref: Reference) -> u32 {
        let current_len = self.elements.len() as u32;
        if delta == 0 {
            return current_len;
        }

        let Some(new_len) = current_len.checked_add(delta) else {
            return u32::MAX;
        };
        if new_len == u32::MAX || self.max.is_some_and(|max| new_len > max) {
            return u32::MAX;
        }

        self.elements.resize(new_len as usize, None);
        if let Some(initial_ref) = initial_ref {
            self.elements[current_len as usize..].fill(Some(initial_ref));
        }
        current_len
    }
}

pub fn check_segment_bounds(min: u32, require_min: u64) -> bool {
    require_min <= u64::from(min)
}

#[cfg(test)]
mod tests {
    use super::{check_segment_bounds, TableInstance};
    use crate::module::{RefType, Table};

    #[test]
    fn new_initializes_minimum_length() {
        let table = Table {
            min: 3,
            max: Some(5),
            ty: RefType::EXTERNREF,
        };

        let instance = TableInstance::new(&table);
        assert_eq!(instance.elements, vec![None, None, None]);
        assert_eq!(instance.min, 3);
        assert_eq!(instance.max, Some(5));
        assert_eq!(instance.ty, RefType::EXTERNREF);
    }

    #[test]
    fn grow_returns_previous_length_and_fills_with_reference() {
        let mut table = TableInstance {
            elements: vec![Some(1), Some(2)],
            min: 2,
            max: Some(10),
            ty: RefType::FUNCREF,
        };

        assert_eq!(table.grow(3, Some(99)), 2);
        assert_eq!(
            table.elements,
            vec![Some(1), Some(2), Some(99), Some(99), Some(99)]
        );
        assert_eq!(table.grow(0, Some(99)), 5);
        assert_eq!(
            table.elements,
            vec![Some(1), Some(2), Some(99), Some(99), Some(99)]
        );
    }

    #[test]
    fn grow_returns_minus_one_on_bounds_failure() {
        let mut table = TableInstance {
            elements: vec![None; 4],
            min: 4,
            max: Some(5),
            ty: RefType::FUNCREF,
        };

        assert_eq!(table.grow(2, None), u32::MAX);
        assert_eq!(table.elements.len(), 4);

        table.elements = vec![None; 16];
        table.max = None;
        assert_eq!(table.grow(u32::MAX - 15, None), u32::MAX);
        assert_eq!(table.elements.len(), 16);
    }

    #[test]
    fn check_segment_bounds_matches_go_semantics() {
        assert!(check_segment_bounds(3, 3));
        assert!(check_segment_bounds(3, 2));
        assert!(!check_segment_bounds(3, 4));
        assert!(!check_segment_bounds(0, 1));
    }
}
