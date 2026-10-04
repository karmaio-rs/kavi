//! Deserializes a string or ordered string pairs through Serde.
//!
//! Supports explicit scalar conversions and structured access to named values
//! without interpreting the input as JSON or form data. Callers provide decoded
//! strings; this helper performs no URL decoding or HTTP-specific processing.

use serde::de::{self, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor};

pub(crate) type Error = de::value::Error;

/// Deserializes one string with explicit conversions for scalar types.
pub(crate) struct Scalar<'a>(pub(crate) &'a str);

macro_rules! number {
    ($($method:ident => $ty:ty, $visit:ident);* $(;)?) => {
        $(fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            visitor.$visit(self.0.parse::<$ty>().map_err(de::Error::custom)?)
        })*
    };
}

impl<'de> de::Deserializer<'de> for Scalar<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_borrowed_str(self.0)
    }

    number! {
        deserialize_bool => bool, visit_bool;
        deserialize_i8 => i8, visit_i8;
        deserialize_i16 => i16, visit_i16;
        deserialize_i32 => i32, visit_i32;
        deserialize_i64 => i64, visit_i64;
        deserialize_i128 => i128, visit_i128;
        deserialize_u8 => u8, visit_u8;
        deserialize_u16 => u16, visit_u16;
        deserialize_u32 => u32, visit_u32;
        deserialize_u64 => u64, visit_u64;
        deserialize_u128 => u128, visit_u128;
        deserialize_f32 => f32, visit_f32;
        deserialize_f64 => f64, visit_f64;
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let mut chars = self.0.chars();
        match (chars.next(), chars.next()) {
            (Some(value), None) => visitor.visit_char(value),
            _ => Err(de::Error::custom("expected one character")),
        }
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_borrowed_bytes(self.0.as_bytes())
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_byte_buf(self.0.as_bytes().to_vec())
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _: &'static str, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_enum(self.0.into_deserializer())
    }

    serde::forward_to_deserialize_any! {
        str string unit unit_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}

/// Deserializes ordered name/value pairs as a map, sequence, or single scalar.
pub(crate) struct Pairs<I>(pub(crate) I);

macro_rules! scalar {
    ($($method:ident);* $(;)?) => {
        $(fn $method<V: Visitor<'de>>(mut self, visitor: V) -> Result<V::Value, Error> {
            if self.0.len() == 1 {
                let (_, value) = self.0.next().expect("one value");
                de::Deserializer::$method(Scalar(value), visitor)
            } else {
                Err(de::Error::custom("a scalar requires exactly one value"))
            }
        })*
    };
}

impl<'de, I: ExactSizeIterator<Item = (&'de str, &'de str)>> de::Deserializer<'de> for Pairs<I> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_map(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_map(PairAccess {
            values: self.0,
            pending: None,
        })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_map(visitor)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_seq(Values(self.0))
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, _: usize, visitor: V) -> Result<V::Value, Error> {
        let mut values = Values(self.0);
        let result = visitor.visit_seq(&mut values)?;
        if values.0.next().is_some() {
            return Err(de::Error::custom("too many values for tuple"));
        }
        Ok(result)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        length: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_tuple(length, visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _: &'static str, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.0.len() == 0 {
            visitor.visit_unit()
        } else {
            Err(de::Error::custom("expected no values"))
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(self, _: &'static str, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        mut self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        if self.0.len() == 1 {
            let (_, value) = self.0.next().expect("one value");
            de::Deserializer::deserialize_enum(Scalar(value), name, variants, visitor)
        } else {
            Err(de::Error::custom("an enum requires exactly one value"))
        }
    }

    scalar! {
        deserialize_bool; deserialize_i8; deserialize_i16; deserialize_i32; deserialize_i64;
        deserialize_i128; deserialize_u8; deserialize_u16; deserialize_u32; deserialize_u64;
        deserialize_u128; deserialize_f32; deserialize_f64; deserialize_char;
        deserialize_str; deserialize_string; deserialize_bytes; deserialize_byte_buf;
    }

    serde::forward_to_deserialize_any! { identifier ignored_any }
}

struct PairAccess<'a, I> {
    values: I,
    pending: Option<&'a str>,
}

impl<'de, I: ExactSizeIterator<Item = (&'de str, &'de str)>> MapAccess<'de> for PairAccess<'de, I> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>, Error> {
        match self.values.next() {
            Some((key, value)) => {
                self.pending = Some(value);
                seed.deserialize(key.into_deserializer()).map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let value = self.pending.take().ok_or_else(|| de::Error::custom("missing value"))?;
        seed.deserialize(Scalar(value))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.values.len())
    }
}

struct Values<I>(I);

impl<'de, I: ExactSizeIterator<Item = (&'de str, &'de str)>> SeqAccess<'de> for Values<I> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>, Error> {
        self.0
            .next()
            .map(|(_, value)| seed.deserialize(Scalar(value)))
            .transpose()
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.0.len())
    }
}
