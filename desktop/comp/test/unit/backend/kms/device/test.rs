use super::{calculate_scale, connector::Interface};

#[test]
fn test_scale() {
    fn scale(interface: Interface, diag_in: f64, w_px: u16, h_px: u16) -> i32 {
        let aspect = (w_px as f64) / (h_px as f64);
        let diag_mm = diag_in * 25.4;
        let h_mm = diag_mm / (aspect.powf(2.0) + 1.0).sqrt();
        let w_mm = h_mm * aspect;
        (calculate_scale(interface, (w_mm as u32, h_mm as u32), (w_px, h_px)) * 100.0) as i32
    }

    // Laptops
    for &iface in &[Interface::EmbeddedDisplayPort, Interface::LVDS] {
        // 14 inch 3:2
        assert_eq!(scale(iface, 14.0, 3000, 2000), 200);

        // Various sizes 16:9
        for &diag_in in &[14.0, 15.6, 17.3] {
            assert_eq!(scale(iface, diag_in, 1920, 1080), 100);
            assert_eq!(scale(iface, diag_in, 2560, 1440), 150);
            assert_eq!(scale(iface, diag_in, 3840, 2160), 200);
        }

        // Various sizes 16:10
        for &diag_in in &[14.0, 16.0] {
            assert_eq!(scale(iface, diag_in, 1920, 1200), 100);
            assert_eq!(scale(iface, diag_in, 2560, 1600), 150);
            assert_eq!(scale(iface, diag_in, 3840, 2400), 200);
        }
    }

    // Desktops
    for &iface in &[Interface::DisplayPort, Interface::HDMIA, Interface::HDMIB] {
        // 24 inch 16:9
        assert_eq!(scale(iface, 24.0, 1920, 1080), 100);
        assert_eq!(scale(iface, 24.0, 2560, 1440), 150);
        assert_eq!(scale(iface, 24.0, 3840, 2160), 200);

        // Larger sizes 16:9
        for &diag_in in &[27.0, 32.0, 38.0] {
            assert_eq!(scale(iface, diag_in, 1920, 1080), 100);
            assert_eq!(scale(iface, diag_in, 2560, 1440), 100);
            assert_eq!(scale(iface, diag_in, 3840, 2160), 200);
        }

        // Smaller sizes 21:9
        for &diag_in in &[26.0, 29.0] {
            assert_eq!(scale(iface, diag_in, 2560, 1080), 100);
            assert_eq!(scale(iface, diag_in, 3440, 1440), 150);
            assert_eq!(scale(iface, diag_in, 5120, 2160), 200);
        }

        // Larger sizes 21:9
        for &diag_in in &[34.0, 45.0] {
            assert_eq!(scale(iface, diag_in, 2560, 1080), 100);
            assert_eq!(scale(iface, diag_in, 3440, 1440), 100);
            assert_eq!(scale(iface, diag_in, 5120, 2160), 200);
        }

        // Various sizes 32:9
        for &diag_in in &[45.0, 49.0, 57.0] {
            assert_eq!(scale(iface, diag_in, 3840, 1080), 100);
            assert_eq!(scale(iface, diag_in, 5120, 1440), 100);
            assert_eq!(scale(iface, diag_in, 7680, 2160), 200);
        }
    }

    // TVs
    for &iface in &[Interface::DisplayPort, Interface::HDMIA, Interface::HDMIB] {
        // Various sizes 16:9
        for &diag_in in &[40.0, 42.0, 48.0, 55.0, 65.0, 77.0, 83.0] {
            assert_eq!(scale(iface, diag_in, 1920, 1080), 100);
            assert_eq!(scale(iface, diag_in, 2560, 1440), 200);
            assert_eq!(scale(iface, diag_in, 3840, 2160), 200);
        }
    }

    // Zero sized displays (projectors, invalid EDID)
    for &iface in &[Interface::DisplayPort, Interface::HDMIA, Interface::HDMIB] {
        assert_eq!(scale(iface, 0.0, 1920, 1080), 100);
        assert_eq!(scale(iface, 0.0, 2560, 1440), 100);
        assert_eq!(scale(iface, 0.0, 3840, 2160), 200);
    }
}
