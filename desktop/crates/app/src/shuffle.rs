//! Shuffle and anti-repeat selection engine.
use adult_hub_client::TvItem;
use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

#[derive(Debug)]
pub struct ShuffleEngine {
    history: Mutex<VecDeque<String>>,
    max_history: usize,
}

impl Default for ShuffleEngine {
    fn default() -> Self {
        Self::new(20)
    }
}

impl ShuffleEngine {
    pub fn new(max_history: usize) -> Self {
        Self {
            history: Mutex::new(VecDeque::new()),
            max_history,
        }
    }

    /// Selects a random item from `items`, respecting:
    /// 1. Exclusion of `current_id` if items.len() > 1
    /// 2. Anti-repeat exclusion using recent history (dynamically scaled to library size)
    /// 3. OS entropy / fastrand randomness
    /// Returns None if `items` is empty.
    pub fn select_candidate(
        &self,
        items: &[TvItem],
        current_id: Option<&str>,
    ) -> Option<TvItem> {
        if items.is_empty() {
            return None;
        }

        if items.len() == 1 {
            let choice = items[0].clone();
            self.record_choice(&choice.id);
            return Some(choice);
        }

        let mut history = self.history.lock().unwrap();

        // Dynamically compute usable history length:
        // E.g., for 2 items: 0 (can alternate cleanly).
        // For 3 items: 1.
        // For 4 items: 1.
        // For 10 items: min((10-1)/2, 20) = 4 items.
        // For 100 items: min((100-1)/2, 20) = 20 items.
        let max_exclusions = (items.len().saturating_sub(1) / 2).min(self.max_history);
        let recent_exclusions: HashSet<&str> = history
            .iter()
            .take(max_exclusions)
            .map(|s| s.as_str())
            .collect();

        // Level 1: Exclude both current_id and recent history
        let candidates: Vec<&TvItem> = items
            .iter()
            .filter(|item| {
                if let Some(cur) = current_id {
                    if item.id == cur {
                        return false;
                    }
                }
                !recent_exclusions.contains(item.id.as_str())
            })
            .collect();

        let chosen = if !candidates.is_empty() {
            let idx = fastrand::usize(..candidates.len());
            candidates[idx].clone()
        } else {
            // Level 2: Relax recent history, only exclude current_id
            let relaxed: Vec<&TvItem> = items
                .iter()
                .filter(|item| {
                    if let Some(cur) = current_id {
                        item.id != cur
                    } else {
                        true
                    }
                })
                .collect();

            if !relaxed.is_empty() {
                let idx = fastrand::usize(..relaxed.len());
                relaxed[idx].clone()
            } else {
                // Fallback: pick any item randomly
                let idx = fastrand::usize(..items.len());
                items[idx].clone()
            }
        };

        // Record to history (push front, evict oldest)
        history.retain(|id| id != &chosen.id);
        history.push_front(chosen.id.clone());
        while history.len() > self.max_history {
            history.pop_back();
        }

        Some(chosen)
    }

    pub fn record_choice(&self, id: &str) {
        let mut history = self.history.lock().unwrap();
        history.retain(|i| i != id);
        history.push_front(id.to_string());
        while history.len() > self.max_history {
            history.pop_back();
        }
    }

    pub fn history_len(&self) -> usize {
        self.history.lock().unwrap().len()
    }

    pub fn clear_history(&self) {
        self.history.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_item(id: &str) -> TvItem {
        TvItem {
            id: id.to_string(),
            title: format!("Title {id}"),
            overview: None,
            year: None,
            runtime_ticks: None,
            genres: vec![],
            tags: vec![],
            studios: vec![],
            people: vec![],
            primary_image_tag: None,
            backdrop_image_tag: None,
            thumb_image_tag: None,
            child_count: None,
            is_favorite: false,
            playback_position_ticks: 0,
            played_percentage: None,
            is_played: false,
            provider_video_id: None,
            source_provider: None,
            media_source_id: None,
            container: None,
            file_size_bytes: None,
            width: None,
            height: None,
        }
    }

    #[test]
    fn test_shuffle_empty() {
        let engine = ShuffleEngine::default();
        let items = vec![];
        assert!(engine.select_candidate(&items, None).is_none());
    }

    #[test]
    fn test_shuffle_single_item() {
        let engine = ShuffleEngine::default();
        let items = vec![dummy_item("item-1")];
        let chosen = engine.select_candidate(&items, Some("item-1")).unwrap();
        assert_eq!(chosen.id, "item-1");
    }

    #[test]
    fn test_shuffle_two_items_alternation() {
        let engine = ShuffleEngine::default();
        let items = vec![dummy_item("item-1"), dummy_item("item-2")];

        // If item-1 is current, must pick item-2
        let pick1 = engine.select_candidate(&items, Some("item-1")).unwrap();
        assert_eq!(pick1.id, "item-2");

        // If item-2 is current, must pick item-1
        let pick2 = engine.select_candidate(&items, Some("item-2")).unwrap();
        assert_eq!(pick2.id, "item-1");
    }

    #[test]
    fn test_shuffle_100_picks_large_catalog_distribution_and_no_immediate_repeat() {
        let engine = ShuffleEngine::new(20);
        let items: Vec<TvItem> = (0..200)
            .map(|i| dummy_item(&format!("video-{i}")))
            .collect();

        let mut current_id: Option<String> = None;
        let mut picked_ids = Vec::new();
        let mut unique_picks = HashSet::new();

        for _ in 0..100 {
            let chosen = engine
                .select_candidate(&items, current_id.as_deref())
                .expect("item picked");
            
            // Cannot pick current
            if let Some(ref cur) = current_id {
                assert_ne!(&chosen.id, cur, "Shuffle must never pick current playing item");
            }

            picked_ids.push(chosen.id.clone());
            unique_picks.insert(chosen.id.clone());
            current_id = Some(chosen.id);
        }

        // On 100 picks from 200 items with anti-repeat window of 20,
        // we should have high diversity (at least 70 unique items).
        assert!(
            unique_picks.len() >= 70,
            "Expected high diversity, got {} unique out of 100 picks",
            unique_picks.len()
        );

        // Verify anti-repeat: no item should repeat within a window of 20
        for i in 0..picked_ids.len() {
            let id = &picked_ids[i];
            let window_end = (i + 20).min(picked_ids.len());
            for j in (i + 1)..window_end {
                assert_ne!(
                    id, &picked_ids[j],
                    "Item {} repeated too soon at index {} (first at {})",
                    id, j, i
                );
            }
        }
    }

    #[test]
    fn test_shuffle_few_items() {
        let engine = ShuffleEngine::default();
        let items = vec![
            dummy_item("video-a"),
            dummy_item("video-b"),
            dummy_item("video-c"),
        ];

        let mut cur = Some("video-a".to_string());
        for _ in 0..30 {
            let picked = engine
                .select_candidate(&items, cur.as_deref())
                .expect("item chosen");
            assert_ne!(picked.id, cur.unwrap());
            cur = Some(picked.id);
        }
    }

    #[test]
    fn test_shuffle_restart_non_deterministic() {
        let items: Vec<TvItem> = (0..50)
            .map(|i| dummy_item(&format!("item-{i}")))
            .collect();

        // Run 1:
        let engine1 = ShuffleEngine::default();
        let mut cur1 = None;
        let mut seq1 = Vec::new();
        for _ in 0..30 {
            let pick = engine1.select_candidate(&items, cur1.as_deref()).unwrap();
            cur1 = Some(pick.id.clone());
            seq1.push(pick.id);
        }

        // Run 2 (representing next application start):
        let engine2 = ShuffleEngine::default();
        let mut cur2 = None;
        let mut seq2 = Vec::new();
        for _ in 0..30 {
            let pick = engine2.select_candidate(&items, cur2.as_deref()).unwrap();
            cur2 = Some(pick.id.clone());
            seq2.push(pick.id);
        }

        // Entropy ensures the 30-item sequence is not identical
        assert_ne!(
            seq1, seq2,
            "Sequential application restarts must not generate identical deterministic sequences"
        );
    }

    #[test]
    fn test_shuffle_pagination_across_whole_library() {
        // Library of 1,000 items
        let total_items: Vec<TvItem> = (0..1000)
            .map(|i| dummy_item(&format!("catalog-{i}")))
            .collect();

        let engine = ShuffleEngine::new(20);
        let mut picked_ids = Vec::new();
        let mut current_id: Option<String> = None;

        // Simulate 120 user shuffle selections where Jellyfin queries 50-item pages
        // with randomized offsets for large collections:
        for _ in 0..120 {
            let total = total_items.len();
            let random_offset = fastrand::usize(0..total.saturating_sub(49));
            let page_slice = &total_items[random_offset..(random_offset + 50)];

            let pick = engine
                .select_candidate(page_slice, current_id.as_deref())
                .expect("item picked");
            
            picked_ids.push(pick.id.clone());
            current_id = Some(pick.id);
        }

        // Verify that picks span across the entire 1,000 item range:
        // Beginning (0..200), Middle (400..600), and End (800..1000)
        let has_early = picked_ids.iter().any(|id| {
            let num: usize = id.strip_prefix("catalog-").unwrap().parse().unwrap();
            num < 200
        });
        let has_mid = picked_ids.iter().any(|id| {
            let num: usize = id.strip_prefix("catalog-").unwrap().parse().unwrap();
            (400..600).contains(&num)
        });
        let has_late = picked_ids.iter().any(|id| {
            let num: usize = id.strip_prefix("catalog-").unwrap().parse().unwrap();
            num >= 800
        });

        assert!(has_early, "Picks must include items from the first part of the library");
        assert!(has_mid, "Picks must include items from the middle of the library");
        assert!(has_late, "Picks must include items from the end of the library");
    }
}
