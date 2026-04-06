#![doc = "Linked-list bookkeeping for instantiated store modules."]

use std::collections::BTreeMap;

pub type ModuleInstanceId = u64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModuleLinks {
    pub prev: Option<ModuleInstanceId>,
    pub next: Option<ModuleInstanceId>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoreModuleList {
    head: Option<ModuleInstanceId>,
    links: BTreeMap<ModuleInstanceId, ModuleLinks>,
}

impl StoreModuleList {
    pub fn head(&self) -> Option<ModuleInstanceId> {
        self.head
    }

    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn links(&self, id: ModuleInstanceId) -> Option<ModuleLinks> {
        self.links.get(&id).copied()
    }

    pub fn push_front(&mut self, id: ModuleInstanceId) {
        let previous_head = self.head.replace(id);
        self.links.insert(
            id,
            ModuleLinks {
                prev: None,
                next: previous_head,
            },
        );

        if let Some(old_head) = previous_head {
            if let Some(old_links) = self.links.get_mut(&old_head) {
                old_links.prev = Some(id);
            }
        }
    }

    pub fn remove(&mut self, id: ModuleInstanceId) -> Option<ModuleLinks> {
        let removed = self.links.remove(&id)?;

        if let Some(prev) = removed.prev {
            if let Some(prev_links) = self.links.get_mut(&prev) {
                prev_links.next = removed.next;
            }
        } else if self.head == Some(id) {
            self.head = removed.next;
        }

        if let Some(next) = removed.next {
            if let Some(next_links) = self.links.get_mut(&next) {
                next_links.prev = removed.prev;
            }
        }

        Some(removed)
    }

    pub fn iter(&self) -> StoreModuleListIter<'_> {
        StoreModuleListIter {
            list: self,
            current: self.head,
        }
    }
}

pub struct StoreModuleListIter<'a> {
    list: &'a StoreModuleList,
    current: Option<ModuleInstanceId>,
}

impl Iterator for StoreModuleListIter<'_> {
    type Item = ModuleInstanceId;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.current?;
        self.current = self.list.links(current).and_then(|links| links.next);
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_front_and_remove_match_go_link_order() {
        let mut list = StoreModuleList::default();

        list.push_front(1);
        list.push_front(2);
        list.push_front(3);

        assert_eq!(Some(3), list.head());
        assert_eq!(
            Some(ModuleLinks {
                prev: None,
                next: Some(2)
            }),
            list.links(3)
        );
        assert_eq!(
            Some(ModuleLinks {
                prev: Some(3),
                next: Some(1)
            }),
            list.links(2)
        );
        assert_eq!(
            Some(ModuleLinks {
                prev: Some(2),
                next: None
            }),
            list.links(1)
        );
        assert_eq!(vec![3, 2, 1], list.iter().collect::<Vec<_>>());

        assert_eq!(
            Some(ModuleLinks {
                prev: Some(3),
                next: Some(1)
            }),
            list.remove(2)
        );
        assert_eq!(vec![3, 1], list.iter().collect::<Vec<_>>());
        assert_eq!(
            Some(ModuleLinks {
                prev: None,
                next: Some(1)
            }),
            list.links(3)
        );
        assert_eq!(
            Some(ModuleLinks {
                prev: Some(3),
                next: None
            }),
            list.links(1)
        );
    }
}
