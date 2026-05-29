use std::path::PathBuf;
use std::{env, fs};

fn main() {
	fs::write(
		PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("osstr.txt"),
		format!(
			"{}/{},",
			rustc_version::version().unwrap(),
			env::var("TARGET").unwrap()
		),
	)
	.unwrap();
}
