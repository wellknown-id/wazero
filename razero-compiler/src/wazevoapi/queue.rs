//! Resettable queue with reusable backing storage.

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Queue<T> {
    index: usize,
    data: Vec<T>,
}

impl<T> Queue<T> {
    pub fn enqueue(&mut self, value: T) {
        self.data.push(value);
    }

    pub fn dequeue(&mut self) -> T
    where
        T: Clone,
    {
        let value = self.data[self.index].clone();
        self.index += 1;
        value
    }

    pub fn empty(&self) -> bool {
        self.index >= self.data.len()
    }

    pub fn reset(&mut self) {
        self.index = 0;
        self.data.clear();
    }

    pub fn data(&self) -> &[T] {
        &self.data
    }

    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::Queue;

    #[test]
    fn queue_reuses_storage_after_reset() {
        let mut queue = Queue::default();
        queue.enqueue(1u32);
        queue.enqueue(2u32);
        assert_eq!(queue.dequeue(), 1);
        assert!(!queue.empty());
        assert_eq!(queue.dequeue(), 2);
        assert!(queue.empty());

        let capacity = queue.capacity();
        queue.reset();
        queue.enqueue(3);
        assert_eq!(queue.dequeue(), 3);
        assert_eq!(queue.data().len(), 1);
        assert!(queue.capacity() >= capacity);
    }
}
