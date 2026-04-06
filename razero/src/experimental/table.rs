#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Table {
    minimum: u32,
    maximum: Option<u32>,
}

impl Table {
    pub fn new(minimum: u32, maximum: Option<u32>) -> Self {
        Self { minimum, maximum }
    }

    pub fn minimum(&self) -> u32 {
        self.minimum
    }

    pub fn maximum(&self) -> Option<u32> {
        self.maximum
    }
}
