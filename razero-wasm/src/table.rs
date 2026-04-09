#![doc = "Runtime-side Wasm table instances."]

use std::sync::{Arc, RwLock};

use crate::module::{RefType, Table};
use crate::store_module_list::ModuleInstanceId;

pub type Reference = Option<u64>;
pub type ElementInstance = Vec<Reference>;

pub fn encode_function_reference(module_id: ModuleInstanceId, function_index: u32) -> u64 {
    ((module_id & u64::from(u32::MAX)) << 32) | u64::from(function_index)
}

pub fn decode_function_reference(reference: u64) -> (ModuleInstanceId, u32) {
    (reference >> 32, reference as u32)
}

#[derive(Debug, Clone)]
pub struct TableInstance {
    elements: Arc<RwLock<ElementInstance>>,
    pub min: u32,
    pub max: Option<u32>,
    pub ty: RefType,
}

impl PartialEq for TableInstance {
    fn eq(&self, other: &Self) -> bool {
        self.min == other.min
            && self.max == other.max
            && self.ty == other.ty
            && self.elements() == other.elements()
    }
}

impl Eq for TableInstance {}

impl Default for TableInstance {
    fn default() -> Self {
        Self {
            elements: Arc::new(RwLock::new(Vec::new())),
            min: 0,
            max: None,
            ty: RefType::FUNCREF,
        }
    }
}

impl TableInstance {
    pub fn new(table: &Table) -> Self {
        Self {
            elements: Arc::new(RwLock::new(vec![None; table.min as usize])),
            min: table.min,
            max: table.max,
            ty: table.ty,
        }
    }

    pub fn from_elements(
        elements: ElementInstance,
        min: u32,
        max: Option<u32>,
        ty: RefType,
    ) -> Self {
        Self {
            elements: Arc::new(RwLock::new(elements)),
            min,
            max,
            ty,
        }
    }

    pub fn elements(&self) -> ElementInstance {
        self.elements.read().expect("table read lock").clone()
    }

    pub fn shared_elements(&self) -> Arc<RwLock<ElementInstance>> {
        self.elements.clone()
    }

    pub fn len(&self) -> usize {
        self.elements.read().expect("table read lock").len()
    }

    pub fn get(&self, index: usize) -> Option<Reference> {
        self.elements
            .read()
            .expect("table read lock")
            .get(index)
            .copied()
    }

    pub fn write_range(&self, offset: usize, values: &[Reference]) {
        self.elements.write().expect("table write lock")[offset..offset + values.len()]
            .clone_from_slice(values);
    }

    pub fn grow(&self, delta: u32, initial_ref: Reference) -> u32 {
        let mut elements = self.elements.write().expect("table write lock");
        let current_len = elements.len() as u32;
        if delta == 0 {
            return current_len;
        }

        let Some(new_len) = current_len.checked_add(delta) else {
            return u32::MAX;
        };
        if new_len == u32::MAX || self.max.is_some_and(|max| new_len > max) {
            return u32::MAX;
        }

        elements.resize(new_len as usize, None);
        if let Some(initial_ref) = initial_ref {
            elements[current_len as usize..].fill(Some(initial_ref));
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
        assert_eq!(instance.elements(), vec![None, None, None]);
        assert_eq!(instance.min, 3);
        assert_eq!(instance.max, Some(5));
        assert_eq!(instance.ty, RefType::EXTERNREF);
    }

    #[test]
    fn grow_returns_previous_length_and_fills_with_reference() {
        let table =
            TableInstance::from_elements(vec![Some(1), Some(2)], 2, Some(10), RefType::FUNCREF);

        assert_eq!(table.grow(3, Some(99)), 2);
        assert_eq!(
            table.elements(),
            vec![Some(1), Some(2), Some(99), Some(99), Some(99)]
        );
        assert_eq!(table.grow(0, Some(99)), 5);
        assert_eq!(
            table.elements(),
            vec![Some(1), Some(2), Some(99), Some(99), Some(99)]
        );
    }

    #[test]
    fn grow_returns_minus_one_on_bounds_failure() {
        let mut table = TableInstance::from_elements(vec![None; 4], 4, Some(5), RefType::FUNCREF);

        assert_eq!(table.grow(2, None), u32::MAX);
        assert_eq!(table.len(), 4);

        table = TableInstance::from_elements(vec![None; 16], 4, None, RefType::FUNCREF);
        assert_eq!(table.grow(u32::MAX - 15, None), u32::MAX);
        assert_eq!(table.len(), 16);
    }

    #[test]
    fn check_segment_bounds_matches_go_semantics() {
        assert!(check_segment_bounds(3, 3));
        assert!(check_segment_bounds(3, 2));
        assert!(!check_segment_bounds(3, 4));
        assert!(!check_segment_bounds(0, 1));
    }
}
