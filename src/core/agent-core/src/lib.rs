pub use lha_core::kernel;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexports_kernel_path() {
        let _kernel = kernel::AgentKernel::new();
    }
}
