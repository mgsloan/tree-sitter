use fearless_simd::{Level, i16x32, mask8x32, mask16x16, mask16x32, prelude::*};

pub(crate) trait WordMask<S: Simd>: SimdMask<S> {
    fn slot_bits(self, simd: S) -> u64;
}

impl<S: Simd> WordMask<S> for mask16x16<S> {
    #[inline(always)]
    fn slot_bits(self, _: S) -> u64 {
        self.to_bitmask()
    }
}

impl<S: Simd> WordMask<S> for mask16x32<S> {
    #[inline(always)]
    fn slot_bits(self, simd: S) -> u64 {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if simd.level().as_avx512().is_some() {
            return self.to_bitmask();
        }
        // Narrow before extracting bits to avoid two movemasks and PEXTs on AVX2.
        let values = i16x32::from_slice(simd, &<[i16; 32]>::from(self));
        let (low, high) = values.split();
        mask8x32::from_slice(simd, &low.saturating_narrow(high).to_array()).to_bitmask()
    }
}

#[inline]
pub(crate) fn level() -> Level {
    // The Avx2 token needs all x86-64-v3 features, even in a +avx2 build.
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    if Level::baseline().as_avx2().is_none() {
        return Level::new();
    }
    Level::baseline()
}

#[inline(always)]
pub(crate) fn load_words<S: Simd, V: SimdInt<S>>(simd: S, bytes: &[u8]) -> V
where
    V::Element: From<u8>,
{
    let values = V::from_bytes(V::ByteVector::from_slice(simd, bytes));
    #[cfg(target_endian = "big")]
    let values = (values << 8) | ((values >> 8) & V::Element::from(255));
    values
}

#[cfg(test)]
pub(crate) fn test_levels() -> Vec<Level> {
    let mut levels = vec![Level::baseline()];
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        let available = Level::new();
        if let Some(simd) = available.as_sse2() {
            levels.push(Level::Sse2(simd));
        }
        if let Some(simd) = available.as_sse4_2() {
            levels.push(Level::Sse4_2(simd));
        }
        if let Some(simd) = available.as_avx2() {
            levels.push(Level::Avx2(simd));
        }
    }
    levels.push(Level::new());
    levels
}
