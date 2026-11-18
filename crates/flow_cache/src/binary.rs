//! Minimal positional binary Serde transport for versioned cache payloads.
//!
//! Sequence lengths and strings are checked against the remaining payload before
//! traversal. Sequence access deliberately returns no allocation size hint, so
//! an untrusted count cannot cause an eager `Vec` reservation.

use serde::{
    Serialize,
    de::{
        self, DeserializeOwned, DeserializeSeed, EnumAccess, IntoDeserializer, MapAccess,
        SeqAccess, VariantAccess, Visitor,
    },
    ser::{
        self, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
        SerializeTupleStruct, SerializeTupleVariant,
    },
};
use std::fmt;

#[derive(Debug)]
pub(super) struct Error;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid bounded binary payload")
    }
}
impl std::error::Error for Error {}
impl ser::Error for Error {
    fn custom<T: fmt::Display>(_msg: T) -> Self {
        Self
    }
}
impl de::Error for Error {
    fn custom<T: fmt::Display>(_msg: T) -> Self {
        Self
    }
}

pub(super) fn to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    let mut encoder = Encoder { bytes: Vec::new() };
    value.serialize(&mut encoder)?;
    Ok(encoder.bytes)
}

pub(super) fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    let mut decoder = Decoder { bytes, offset: 0 };
    let value = T::deserialize(&mut decoder)?;
    if decoder.offset != bytes.len() {
        return Err(Error);
    }
    Ok(value)
}

struct Encoder {
    bytes: Vec<u8>,
}
impl Encoder {
    fn varint(&mut self, mut value: u128) {
        while value >= 0x80 {
            self.bytes.push(value as u8 | 0x80);
            value >>= 7;
        }
        self.bytes.push(value as u8);
    }
    fn signed(&mut self, value: i128) {
        self.varint(((value as u128) << 1) ^ ((value >> 127) as u128));
    }
}

impl ser::Serializer for &mut Encoder {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;
    fn serialize_bool(self, v: bool) -> Result<(), Error> {
        self.bytes.push(v as u8);
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<(), Error> {
        self.signed(v as i128);
        Ok(())
    }
    fn serialize_i16(self, v: i16) -> Result<(), Error> {
        self.signed(v as i128);
        Ok(())
    }
    fn serialize_i32(self, v: i32) -> Result<(), Error> {
        self.signed(v as i128);
        Ok(())
    }
    fn serialize_i64(self, v: i64) -> Result<(), Error> {
        self.signed(v as i128);
        Ok(())
    }
    fn serialize_i128(self, v: i128) -> Result<(), Error> {
        self.signed(v);
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<(), Error> {
        self.varint(v as u128);
        Ok(())
    }
    fn serialize_u16(self, v: u16) -> Result<(), Error> {
        self.varint(v as u128);
        Ok(())
    }
    fn serialize_u32(self, v: u32) -> Result<(), Error> {
        self.varint(v as u128);
        Ok(())
    }
    fn serialize_u64(self, v: u64) -> Result<(), Error> {
        self.varint(v as u128);
        Ok(())
    }
    fn serialize_u128(self, v: u128) -> Result<(), Error> {
        self.varint(v);
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<(), Error> {
        self.bytes.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_f64(self, v: f64) -> Result<(), Error> {
        self.bytes.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result<(), Error> {
        self.serialize_u32(v as u32)
    }
    fn serialize_str(self, v: &str) -> Result<(), Error> {
        self.varint(v.len() as u128);
        self.bytes.extend_from_slice(v.as_bytes());
        Ok(())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), Error> {
        self.varint(v.len() as u128);
        self.bytes.extend_from_slice(v);
        Ok(())
    }
    fn serialize_none(self) -> Result<(), Error> {
        self.bytes.push(0);
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<(), Error> {
        self.bytes.push(1);
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Error> {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Error> {
        Ok(())
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        index: u32,
        _: &'static str,
    ) -> Result<(), Error> {
        self.serialize_u32(index)
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        i: u32,
        _: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        self.serialize_u32(i)?;
        v.serialize(self)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self, Error> {
        self.varint(len.ok_or(Error)? as u128);
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, Error> {
        Ok(self)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, Error> {
        Ok(self)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        i: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, Error> {
        self.serialize_u32(i)?;
        Ok(self)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self, Error> {
        self.varint(len.ok_or(Error)? as u128);
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, Error> {
        Ok(self)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        i: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, Error> {
        self.serialize_u32(i)?;
        Ok(self)
    }
    fn collect_str<T: ?Sized + fmt::Display>(self, v: &T) -> Result<(), Error> {
        self.serialize_str(&v.to_string())
    }
}
macro_rules! compound {
    ($trait:ident,$method:ident) => {
        impl<'a> $trait for &'a mut Encoder {
            type Ok = ();
            type Error = Error;
            fn $method<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
                v.serialize(&mut **self)
            }
            fn end(self) -> Result<(), Error> {
                Ok(())
            }
        }
    };
}
compound!(SerializeSeq, serialize_element);
compound!(SerializeTuple, serialize_element);
compound!(SerializeTupleStruct, serialize_field);
compound!(SerializeTupleVariant, serialize_field);
impl SerializeMap for &mut Encoder {
    type Ok = ();
    type Error = Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        v.serialize(&mut **self)
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Error> {
        Ok(())
    }
}
impl SerializeStruct for &mut Encoder {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Error> {
        Ok(())
    }
}
impl SerializeStructVariant for &mut Encoder {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _: &'static str,
        v: &T,
    ) -> Result<(), Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Error> {
        Ok(())
    }
}

struct Decoder<'de> {
    bytes: &'de [u8],
    offset: usize,
}
impl<'de> Decoder<'de> {
    fn take(&mut self, n: usize) -> Result<&'de [u8], Error> {
        let end = self.offset.checked_add(n).ok_or(Error)?;
        let v = self.bytes.get(self.offset..end).ok_or(Error)?;
        self.offset = end;
        Ok(v)
    }
    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn varint(&mut self) -> Result<u128, Error> {
        let mut v = 0u128;
        for shift in (0..=126).step_by(7) {
            let b = self.byte()?;
            if shift == 126 && b & 0x7c != 0 {
                return Err(Error);
            }
            v |= ((b & 0x7f) as u128) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(Error)
    }
    fn len(&mut self) -> Result<usize, Error> {
        let n = usize::try_from(self.varint()?).map_err(|_| Error)?;
        if n > self.bytes.len().saturating_sub(self.offset) {
            return Err(Error);
        }
        Ok(n)
    }
    fn signed(&mut self) -> Result<i128, Error> {
        let v = self.varint()?;
        Ok(((v >> 1) as i128) ^ -((v & 1) as i128))
    }
}

struct CountSeq<'a, 'de> {
    de: &'a mut Decoder<'de>,
    remaining: usize,
}
impl<'de> SeqAccess<'de> for CountSeq<'_, 'de> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        seed.deserialize(&mut *self.de).map(Some)
    }
    fn size_hint(&self) -> Option<usize> {
        None
    }
}
struct FixedSeq<'a, 'de> {
    de: &'a mut Decoder<'de>,
    remaining: usize,
}
impl<'de> SeqAccess<'de> for FixedSeq<'_, 'de> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        seed.deserialize(&mut *self.de).map(Some)
    }
    fn size_hint(&self) -> Option<usize> {
        None
    }
}
struct CountMap<'a, 'de> {
    de: &'a mut Decoder<'de>,
    remaining: usize,
    value: bool,
}
impl<'de> MapAccess<'de> for CountMap<'_, 'de> {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.value = true;
        seed.deserialize(&mut *self.de).map(Some)
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        if !self.value {
            return Err(Error);
        }
        self.value = false;
        self.remaining -= 1;
        seed.deserialize(&mut *self.de)
    }
    fn size_hint(&self) -> Option<usize> {
        None
    }
}
struct Enum<'a, 'de> {
    de: &'a mut Decoder<'de>,
    variant: u32,
}
impl<'de> EnumAccess<'de> for Enum<'_, 'de> {
    type Error = Error;
    type Variant = Self;
    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self), Error> {
        let v = seed.deserialize(self.variant.into_deserializer())?;
        Ok((v, self))
    }
}
impl<'de> VariantAccess<'de> for Enum<'_, 'de> {
    type Error = Error;
    fn unit_variant(self) -> Result<(), Error> {
        Ok(())
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(self.de)
    }
    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_seq(FixedSeq {
            de: self.de,
            remaining: len,
        })
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_seq(FixedSeq {
            de: self.de,
            remaining: fields.len(),
        })
    }
}

impl<'de> de::Deserializer<'de> for &mut Decoder<'de> {
    type Error = Error;
    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        Err(Error)
    }
    fn deserialize_bool<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        match self.byte()? {
            0 => v.visit_bool(false),
            1 => v.visit_bool(true),
            _ => Err(Error),
        }
    }
    fn deserialize_i8<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_i8(i8::try_from(self.signed()?).map_err(|_| Error)?)
    }
    fn deserialize_i16<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_i16(i16::try_from(self.signed()?).map_err(|_| Error)?)
    }
    fn deserialize_i32<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_i32(i32::try_from(self.signed()?).map_err(|_| Error)?)
    }
    fn deserialize_i64<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_i64(i64::try_from(self.signed()?).map_err(|_| Error)?)
    }
    fn deserialize_i128<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_i128(self.signed()?)
    }
    fn deserialize_u8<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_u8(u8::try_from(self.varint()?).map_err(|_| Error)?)
    }
    fn deserialize_u16<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_u16(u16::try_from(self.varint()?).map_err(|_| Error)?)
    }
    fn deserialize_u32<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_u32(u32::try_from(self.varint()?).map_err(|_| Error)?)
    }
    fn deserialize_u64<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_u64(u64::try_from(self.varint()?).map_err(|_| Error)?)
    }
    fn deserialize_u128<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_u128(self.varint()?)
    }
    fn deserialize_f32<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_f32(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn deserialize_f64<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_f64(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn deserialize_char<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        char::from_u32(u32::try_from(self.varint()?).map_err(|_| Error)?)
            .ok_or(Error)
            .and_then(|c| v.visit_char(c))
    }
    fn deserialize_str<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        let n = self.len()?;
        let s = std::str::from_utf8(self.take(n)?).map_err(|_| Error)?;
        v.visit_borrowed_str(s)
    }
    fn deserialize_string<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_str(v)
    }
    fn deserialize_bytes<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        let n = self.len()?;
        v.visit_borrowed_bytes(self.take(n)?)
    }
    fn deserialize_byte_buf<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        let n = self.len()?;
        v.visit_byte_buf(self.take(n)?.to_vec())
    }
    fn deserialize_option<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        match self.byte()? {
            0 => v.visit_none(),
            1 => v.visit_some(self),
            _ => Err(Error),
        }
    }
    fn deserialize_unit<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_unit()
    }
    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        v: V,
    ) -> Result<V::Value, Error> {
        v.visit_unit()
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        v: V,
    ) -> Result<V::Value, Error> {
        v.visit_newtype_struct(self)
    }
    fn deserialize_seq<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        let n = self.len()?;
        v.visit_seq(CountSeq {
            de: self,
            remaining: n,
        })
    }
    fn deserialize_tuple<V: Visitor<'de>>(self, n: usize, v: V) -> Result<V::Value, Error> {
        v.visit_seq(FixedSeq {
            de: self,
            remaining: n,
        })
    }
    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        n: usize,
        v: V,
    ) -> Result<V::Value, Error> {
        v.visit_seq(FixedSeq {
            de: self,
            remaining: n,
        })
    }
    fn deserialize_map<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        let n = self.len()?;
        v.visit_map(CountMap {
            de: self,
            remaining: n,
            value: false,
        })
    }
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        f: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        v.visit_seq(FixedSeq {
            de: self,
            remaining: f.len(),
        })
    }
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Error> {
        let variant = u32::try_from(self.varint()?).map_err(|_| Error)?;
        v.visit_enum(Enum { de: self, variant })
    }
    fn deserialize_identifier<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_u32(v)
    }
    fn deserialize_ignored_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        Err(Error)
    }
}
