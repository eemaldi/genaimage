use serde_json::{Number, Value};

pub(crate) fn is_integer(number: &Number) -> bool {
    Decimal::parse(number).is_integer()
}

pub(crate) fn json_values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            compare_numbers(left, right) == Ordering::Equal
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| json_values_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left)| {
                    right
                        .get(key)
                        .is_some_and(|right| json_values_equal(left, right))
                })
        }
        _ => left == right,
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

pub(crate) fn compare_numbers(left: &Number, right: &Number) -> Ordering {
    Decimal::parse(left).compare(&Decimal::parse(right))
}

#[derive(Debug)]
pub(crate) struct Decimal {
    negative: bool,
    digits: String,
    exponent: i64,
    zero: bool,
}

impl Decimal {
    fn parse(number: &Number) -> Self {
        Self::try_parse(number).expect("numeric representation was checked")
    }

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

    fn compare(&self, other: &Self) -> Ordering {
        match (self.zero, other.zero) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if other.negative {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, true) => {
                return if self.negative {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, false) => {}
        }
        if self.negative != other.negative {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let magnitude = self.compare_magnitude(other);
        if self.negative {
            magnitude.reverse()
        } else {
            magnitude
        }
    }

    fn compare_magnitude(&self, other: &Self) -> Ordering {
        let left_position = self
            .exponent
            .saturating_add(i64::try_from(self.digits.len()).expect("JSON number length fits i64"));
        let right_position = other.exponent.saturating_add(
            i64::try_from(other.digits.len()).expect("JSON number length fits i64"),
        );
        if left_position != right_position {
            return if left_position < right_position {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let width = self.digits.len().max(other.digits.len());
        for index in 0..width {
            let left = self.digits.as_bytes().get(index).copied().unwrap_or(b'0');
            let right = other.digits.as_bytes().get(index).copied().unwrap_or(b'0');
            if left != right {
                return if left < right {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
        }
        Ordering::Equal
    }
}
