/// NetCDF utilities for reading radar data

use crate::{Result, RadishError};

/// Read a string attribute from a NetCDF file or variable
pub fn read_string_attribute(
    attrs: impl Iterator<Item = netcdf::Attribute>,
    name: &str,
) -> Option<String> {
    attrs
        .find(|a| a.name() == name)
        .and_then(|a| a.value().ok())
        .and_then(|v| match v {
            netcdf::AttrValue::Str(s) => Some(s),
            netcdf::AttrValue::Uchar(u) => Some(String::from_utf8_lossy(&u).to_string()),
            _ => None,
        })
}

/// Read a numeric attribute from a NetCDF file or variable
pub fn read_numeric_attribute<T: netcdf::Numeric>(
    attrs: impl Iterator<Item = netcdf::Attribute>,
    name: &str,
) -> Option<T> {
    attrs
        .find(|a| a.name() == name)
        .and_then(|a| a.value().ok())
        .and_then(|v| match v {
            netcdf::AttrValue::Int(i) => Some(T::from(i).ok()?),
            netcdf::AttrValue::Float(f) => Some(T::from(f).ok()?),
            netcdf::AttrValue::Double(d) => Some(T::from(d).ok()?),
            _ => None,
        })
}
