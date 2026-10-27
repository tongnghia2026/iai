//! What iAi asks of the machine: memory, cores and disk space (`hw`),
//! writing a file in one step (`atomic_file`), the process's memory use
//! (`mem_report`), the folder its files are kept in (`app_data`), the system
//! clipboard's images (`clipboard`) and, on Windows, Explorer's file types
//! and shortcuts (`windows_integration`).

pub mod app_data;
pub mod atomic_file;
pub mod clipboard;
pub mod hw;
pub mod mem_report;
#[cfg(windows)]
pub mod windows_integration;
