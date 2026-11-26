//! Случайные числа симуляции (`IRobustRandom` Robust). Генератор — xoshiro256++: быстрый,
//! с воспроизводимой последовательностью по зерну, что нужно тестам и повторам раундов.

use bevy_ecs::resource::Resource;

/// Генератор случайных чисел симуляции. Ресурс `World`.
#[derive(Resource, Debug, Clone)]
pub struct GameRng {
    state: [u64; 4],
}

impl Default for GameRng {
    /// Зерно из часов и адреса в памяти: разное при каждом запуске.
    fn default() -> Self {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos() as u64);
        let marker = 0u8;
        Self::seeded(time ^ (&raw const marker as u64).rotate_left(32))
    }
}

impl GameRng {
    /// Генератор с заданным зерном: одна и та же последовательность при каждом запуске.
    pub fn seeded(seed: u64) -> Self {
        // SplitMix64 разворачивает зерно в состояние без нулевых слов.
        let mut x = seed;
        let mut next = || {
            x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        Self {
            state: [next(), next(), next(), next()],
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let [s0, s1, s2, s3] = &mut self.state;
        let result = s0.wrapping_add(*s3).rotate_left(23).wrapping_add(*s0);
        let t = *s1 << 17;
        *s2 ^= *s0;
        *s3 ^= *s1;
        *s1 ^= *s2;
        *s0 ^= *s3;
        *s2 ^= t;
        *s3 = s3.rotate_left(45);
        result
    }

    /// Число из `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Число из `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Целое из `[0, bound)`; при `bound == 0` — ноль.
    pub fn below(&mut self, bound: u32) -> u32 {
        // Умножение со сдвигом: без деления и без заметного смещения для игровых диапазонов.
        ((u64::from(self.next_u64() as u32) * u64::from(bound)) >> 32) as u32
    }

    /// Целое из `[min, max)`; при `max <= min` — `min`.
    pub fn range(&mut self, min: i32, max: i32) -> i32 {
        if max <= min {
            return min;
        }
        let span = (i64::from(max) - i64::from(min)) as u32;
        (i64::from(min) + i64::from(self.below(span))) as i32
    }

    /// Число из `[min, max)`.
    pub fn range_f32(&mut self, min: f32, max: f32) -> f32 {
        min + (max - min) * self.next_f32()
    }

    /// `true` с вероятностью `chance` (от 0 до 1).
    pub fn prob(&mut self, chance: f32) -> bool {
        self.next_f32() < chance
    }

    /// Случайный элемент среза.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            return None;
        }
        items.get(self.below(items.len() as u32) as usize)
    }

    /// Вынимает случайный элемент из вектора (`PickAndTake` Robust).
    pub fn pick_and_take<T>(&mut self, items: &mut Vec<T>) -> Option<T> {
        if items.is_empty() {
            return None;
        }
        let index = self.below(items.len() as u32) as usize;
        Some(items.swap_remove(index))
    }

    /// Перемешивает срез (алгоритм Фишера — Йетса).
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            let other = self.below(index as u32 + 1) as usize;
            items.swap(index, other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_sequence_is_reproducible_and_in_range() {
        let mut a = GameRng::seeded(42);
        let mut b = GameRng::seeded(42);
        let mut c = GameRng::seeded(43);
        let first: Vec<u64> = (0..8).map(|_| a.next_u64()).collect();
        assert_eq!(first, (0..8).map(|_| b.next_u64()).collect::<Vec<_>>());
        assert_ne!(first, (0..8).map(|_| c.next_u64()).collect::<Vec<_>>());

        let mut counts = [0u32; 6];
        for _ in 0..60_000 {
            let roll = a.range(1, 7);
            assert!((1..7).contains(&roll));
            counts[(roll - 1) as usize] += 1;
            let unit = a.next_f32();
            assert!((0.0..1.0).contains(&unit));
            assert!((-2.0..3.0).contains(&a.range_f32(-2.0, 3.0)));
        }
        // Равномерность: каждая грань выпадает около 10 000 раз.
        assert!(
            counts.iter().all(|&count| (9_500..10_500).contains(&count)),
            "{counts:?}"
        );
        assert_eq!(a.range(5, 5), 5);
        assert_eq!(a.below(0), 0);
    }

    #[test]
    fn collection_helpers() {
        let mut rng = GameRng::seeded(7);
        assert_eq!(rng.pick::<u8>(&[]), None);
        assert!([1, 2, 3].contains(rng.pick(&[1, 2, 3]).unwrap()));
        let mut items: Vec<u32> = (0..50).collect();
        rng.shuffle(&mut items);
        assert_ne!(items, (0..50).collect::<Vec<_>>());
        let mut sorted = items.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>());
        let taken = rng.pick_and_take(&mut items).unwrap();
        assert!(!items.contains(&taken) && items.len() == 49);
        let hits = (0..10_000).filter(|_| rng.prob(0.25)).count();
        assert!((2_200..2_800).contains(&hits), "{hits}");
        assert!(!rng.prob(0.0) && rng.prob(1.0));
    }
}
