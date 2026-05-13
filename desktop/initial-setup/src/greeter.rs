use zbus::proxy;

#[proxy(
    interface = "com.clawos.Greeter",
    default_service = "com.clawos.Greeter",
    default_path = "/com/clawos/Greeter"
)]
pub trait Greeter {
    async fn initial_setup_end(&mut self, new_user: String) -> Result<(), zbus::Error>;
}
