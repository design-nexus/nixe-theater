//! Whole-title coordinates for a provider's ordered media parts.
use crate::ProviderError;
pub fn locate(durations: &[u64], position: u64) -> Result<(usize, u64, u64), ProviderError> {
    if durations.is_empty() || durations.contains(&0) {
        return Err(ProviderError::InvalidResponse);
    }
    let total = durations.iter().try_fold(0u64, |sum, value| sum.checked_add(*value)).ok_or(ProviderError::InvalidResponse)?;
    let position = position.min(total - 1);
    let mut start = 0;
    for (index, duration) in durations.iter().enumerate() {
        if position < start + duration {
            return Ok((index, start, total));
        }
        start += duration;
    }
    Err(ProviderError::InvalidResponse)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundaries() {
        assert_eq!(locate(&[10, 20], 10).unwrap(), (1, 10, 30));
        assert_eq!(locate(&[10, 20], 99).unwrap(), (1, 10, 30));
        assert!(locate(&[0], 0).is_err());
    }
}
