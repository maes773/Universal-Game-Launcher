fn main() {
    println!("cargo:rerun-if-changed=assets/ugl.ico");
    println!("cargo:rerun-if-changed=assets/ugl.png");
    println!("cargo:rerun-if-changed=ui/main.slint");

    // Las imágenes de la interfaz se incrustan en el ejecutable: ugl.exe funciona suelto.
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent-dark".into())
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/main.slint", config)
        .expect("no se pudo compilar la interfaz Slint");

    embed_windows_icon();
}

/// Icono provisional de UGL para el archivo .exe (el que se ve en el Explorador y la barra de tareas).
#[cfg(windows)]
fn embed_windows_icon() {
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/ugl.ico");
    resource.set("ProductName", "UGL - Universal Game Launcher");
    resource.set("FileDescription", "UGL - Universal Game Launcher");
    if let Err(err) = resource.compile() {
        // Sin el compilador de recursos de Windows el programa funciona igual, solo que sin icono.
        println!("cargo:warning=No se pudo incrustar el icono del .exe: {err}");
    }
}

#[cfg(not(windows))]
fn embed_windows_icon() {}
