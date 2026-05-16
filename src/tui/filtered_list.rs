//! A list that maintains a filtered view (visible indices) over an underlying `Vec<T>`.
//!
//! Used by the TUI to keep loaded data and a fuzzy-filtered subset in sync
//! without duplicating data.

pub struct FilteredList<T> {
    items: Vec<T>,
    visible: Vec<usize>,
}

impl<T> FilteredList<T> {
    pub fn new(items: Vec<T>) -> Self {
        let visible = (0..items.len()).collect();
        Self { items, visible }
    }

    /// Replace all items. Resets the visible set to "all items".
    pub fn set_items(&mut self, items: Vec<T>) {
        self.visible = (0..items.len()).collect();
        self.items = items;
    }

    /// Rebuild the visible set. Items for which `pred` returns true are visible.
    pub fn refilter<F: FnMut(&T) -> bool>(&mut self, mut pred: F) {
        self.visible = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, t)| pred(t).then_some(i))
            .collect();
    }

    /// Make all items visible (clear any filter).
    pub fn show_all(&mut self) {
        self.visible = (0..self.items.len()).collect();
    }

    /// Number of visible items.
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// Iterate visible items in display order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.visible.iter().filter_map(|&i| self.items.get(i))
    }

    /// Get the visible item at `visible_idx` (i.e. as the user sees them).
    pub fn get(&self, visible_idx: usize) -> Option<&T> {
        self.visible
            .get(visible_idx)
            .and_then(|&i| self.items.get(i))
    }

    /// Access the full underlying slice, including items hidden by the filter.
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Position (within the visible list) of the first item matching `pred`.
    pub fn position<F: FnMut(&T) -> bool>(&self, mut pred: F) -> Option<usize> {
        self.visible
            .iter()
            .position(|&i| self.items.get(i).is_some_and(&mut pred))
    }
}

impl<T> Default for FilteredList<T> {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_is_empty() {
        let list: FilteredList<i32> = FilteredList::default();
        assert_eq!(list.len(), 0);
        assert!(list.is_empty());
        assert_eq!(list.get(0), None);
    }

    #[test]
    fn new_makes_all_visible() {
        let list = FilteredList::new(vec![1, 2, 3]);
        assert_eq!(list.len(), 3);
        assert_eq!(list.iter().copied().collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(list.get(0), Some(&1));
        assert_eq!(list.get(2), Some(&3));
        assert_eq!(list.get(3), None);
    }

    #[test]
    fn refilter_restricts_visible() {
        let mut list = FilteredList::new(vec![1, 2, 3, 4, 5]);
        list.refilter(|n| n % 2 == 0);
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().copied().collect::<Vec<_>>(), vec![2, 4]);
        assert_eq!(list.get(0), Some(&2));
        assert_eq!(list.get(1), Some(&4));
        assert_eq!(list.get(2), None);
    }

    #[test]
    fn refilter_with_no_matches_yields_empty_view() {
        let mut list = FilteredList::new(vec![1, 2, 3]);
        list.refilter(|_| false);
        assert_eq!(list.len(), 0);
        assert!(list.is_empty());
        // Underlying items are preserved.
        assert_eq!(list.items(), &[1, 2, 3]);
    }

    #[test]
    fn show_all_restores_visibility() {
        let mut list = FilteredList::new(vec![1, 2, 3]);
        list.refilter(|n| *n == 2);
        assert_eq!(list.len(), 1);
        list.show_all();
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn set_items_resets_filter() {
        let mut list = FilteredList::new(vec![1, 2, 3]);
        list.refilter(|n| *n == 2);
        list.set_items(vec![10, 20]);
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().copied().collect::<Vec<_>>(), vec![10, 20]);
    }

    #[test]
    fn position_returns_visible_index() {
        let mut list = FilteredList::new(vec![10, 20, 30, 40, 50]);
        list.refilter(|n| n % 20 == 0);
        // Visible items are [20, 40].
        assert_eq!(list.position(|n| *n == 20), Some(0));
        assert_eq!(list.position(|n| *n == 40), Some(1));
        // 30 exists in items but isn't visible.
        assert_eq!(list.position(|n| *n == 30), None);
        assert_eq!(list.position(|n| *n == 999), None);
    }

    #[test]
    fn items_returns_underlying_slice() {
        let mut list = FilteredList::new(vec![1, 2, 3]);
        list.refilter(|n| *n == 2);
        assert_eq!(list.items(), &[1, 2, 3]);
    }
}
