use serde_json::{Number, Value};

pub(crate) fn is_integer(number: &Number) -> Option<bool> {
    Decimal::try_parse(number).map(|value| value.is_integer())
}

pub(crate) fn json_values_equal(left: &Value, right: &Value) -> Result<bool, ()> {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            Ok(compare_numbers(left, right)? == Ordering::Equal)
        }
        (Value::Array(left), Value::Array(right)) => {
            if left.len() != right.len() {
                return Ok(false);
            }
            for (left, right) in left.iter().zip(right) {
                if !json_values_equal(left, right)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::Object(left), Value::Object(right)) => {
            if left.len() != right.len() {
                return Ok(false);
            }
            for (key, left) in left {
                let Some(right) = right.get(key) else {
                    return Ok(false);
                };
                if !json_values_equal(left, right)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => Ok(left == right),
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Ordering {
    Less,
    Equal,
    Greater,
}

impl Ordering {
    pub(crate) const fn is_lt(self) -> bool {
        matches!(self, Self::Less)
    }

    pub(crate) const fn is_gt(self) -> bool {
        matches!(self, Self::Greater)
    }

    const fn reverse(self) -> Self {
        match self {
            Self::Less => Self::Greater,
            Self::Equal => Self::Equal,
            Self::Greater => Self::Less,
        }
    }
}

pub(crate) fn compare_numbers(left: &Number, right: &Number) -> Result<Ordering, ()> {
    let left = Decimal::try_parse(left).ok_or(())?;
    let right = Decimal::try_parse(right).ok_or(())?;
    left.compare(&right)
}

#[derive(Debug)]
pub(crate) struct Decimal {
    negative: bool,
    digits: String,
    exponent: i64,
    zero: bool,
}

impl Decimal {
    pub(crate) fn try_parse(number: &Number) -> Option<Self> {
        let source = number.to_string();
        let (negative, unsigned) = source
            .strip_prefix('-')
            .map(|value| (true, value))
            .unwrap_or((false, source.as_str()));
        let (mantissa, exponent) =
            if let Some((mantissa, exponent)) = unsigned.split_once(['e', 'E']) {
                (mantissa, exponent.parse::<i64>().ok()?)
            } else {
                (unsigned, 0_i64)
            };
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let mut digits = format!("{whole}{fraction}");
        let mut exponent = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
        let leading = digits.bytes().take_while(|byte| *byte == b'0').count();
        digits.drain(..leading);
        let zero = digits.is_empty();
        if zero {
            digits.push('0');
            return Some(Self {
                negative: false,
                digits,
                exponent: 0,
                zero,
            });
        }
        let trailing = digits
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'0')
            .count();
        if trailing != 0 {
            digits.truncate(digits.len() - trailing);
            exponent = exponent.checked_add(i64::try_from(trailing).ok()?)?;
        }
        exponent.checked_add(i64::try_from(digits.len()).ok()?)?;
        Some(Self {
            negative,
            digits,
            exponent,
            zero,
        })
    }

    pub(crate) fn is_integer(&self) -> bool {
        self.zero || self.exponent >= 0
    }

    fn compare(&self, other: &Self) -> Result<Ordering, ()> {
        match (self.zero, other.zero) {
            (true, true) => return Ok(Ordering::Equal),
            (true, false) => {
                return Ok(if other.negative {
                    Ordering::Greater
                } else {
                    Ordering::Less
                })
            }
            (false, true) => {
                return Ok(if self.negative {
                    Ordering::Less
                } else {
                    Ordering::Greater
                })
            }
            (false, false) => {}
        }
        if self.negative != other.negative {
            return Ok(if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        }
        let magnitude = self.compare_magnitude(other)?;
        Ok(if self.negative {
            magnitude.reverse()
        } else {
            magnitude
        })
    }

    fn compare_magnitude(&self, other: &Self) -> Result<Ordering, ()> {
        let self_digits = i64::try_from(self.digits.len()).map_err(|_| ())?;
        let other_digits = i64::try_from(other.digits.len()).map_err(|_| ())?;
        let left_position = self.exponent.checked_add(self_digits).ok_or(())?;
        let right_position = other.exponent.checked_add(other_digits).ok_or(())?;
        if left_position != right_position {
            return Ok(if left_position < right_position {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        }
        let width = self.digits.len().max(other.digits.len());
        for index in 0..width {
            let left = self.digits.as_bytes().get(index).copied().unwrap_or(b'0');
            let right = other.digits.as_bytes().get(index).copied().unwrap_or(b'0');
            if left != right {
                return Ok(if left < right {
                    Ordering::Less
                } else {
                    Ordering::Greater
                });
            }
        }
        Ok(Ordering::Equal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extreme_exponents_fail_without_panicking() {
        let supported: Number = "1e999999999999999999".parse().unwrap();
        let unsupported: Number = "1e9999999999999999999".parse().unwrap();
        assert!(compare_numbers(&supported, &supported).is_ok());
        assert!(compare_numbers(&unsupported, &supported).is_err());
    }
}
