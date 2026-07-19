//! Domínios centrais do tuipe.
//!
//! A UI e a persistência ficam intencionalmente fora de [`typing`]: o motor de
//! digitação é um redutor determinístico verificável contra fixtures do Monkeytype.

pub mod adaptive;
pub mod content;
pub mod gamification;
pub mod persistence;
pub mod typing;
pub mod ui;
