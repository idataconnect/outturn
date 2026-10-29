//! An amount as the API hands it to a page.

use serde::Serialize;

use outturn_core::egress::amount;

/// The number, what it is in, and how a person reads it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Money {
    pub amount: i64,
    pub currency: String,
    pub minor_units: u8,
    /// `£50.00`, for what the model is told and a browser that does not know
    /// the currency; a page formats `amount` in the reader's locale.
    pub display: String,
    /// `50`: the amount as a form shows it to be typed again.
    pub figure: String,
}

impl Money {
    pub fn of(amount: i64, currency: &str, minor_units: u8) -> Self {
        Self {
            amount,
            currency: currency.to_string(),
            minor_units,
            display: amount::format(amount, currency, minor_units),
            figure: amount::figure(amount, minor_units),
        }
    }

    /// An amount kept for a whole currency, in its standard units.
    pub fn standard(amount: i64, currency: &str) -> Self {
        Self::of(amount, currency, amount::standard_minor_units(currency))
    }
}
