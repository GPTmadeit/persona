use nih_plug::prelude::*;

/// `persona-standalone master ...` opens Persona Master; anything else opens Persona.
fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("master") {
        args.remove(1);
        nih_export_standalone_with_args::<persona::master::PersonaMaster, _>(args);
    } else {
        nih_export_standalone::<persona::Persona>();
    }
}
