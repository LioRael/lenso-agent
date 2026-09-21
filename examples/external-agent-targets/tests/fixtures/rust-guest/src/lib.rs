wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

struct GuestComponent;

lenso_guest_sdk::guest_request_plugin! {
impl Guest for GuestComponent {
    provides: {
        capability_id: "test.echo@1",
        descriptor_version: "1.0.0",
        requests: ["echo", "fail", "trap", "loop"],
    }
    fn invoke(
        capability: String,
        operation: String,
        request_json: String,
    ) -> Result<String, String> {
        assert_eq!(capability, "test.echo@1");
        match operation.as_str() {
            "fail" => Err("\"declared\"".to_owned()),
            "trap" => panic!("guest trap"),
            "loop" => loop {
                std::hint::spin_loop();
            },
            _ => Ok(request_json),
        }
    }
}
}

export!(GuestComponent);
