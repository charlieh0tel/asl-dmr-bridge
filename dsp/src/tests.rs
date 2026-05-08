use super::dB;

#[test]
fn unity_is_one() {
    assert_eq!(dB::UNITY.linear(), 1.0);
    assert_eq!(dB(0.0).linear(), 1.0);
}

#[test]
fn linear_round_trip() {
    let cases = [-20.0_f32, -6.0, -3.0, 3.0, 6.0, 20.0];
    for db in cases {
        let g = dB(db).linear();
        let back = 20.0 * g.log10();
        assert!((back - db).abs() < 1e-4, "{db} dB -> {g} -> {back}");
    }
}

#[test]
fn chip_byte_rounds_and_clamps() {
    assert_eq!(dB(0.0).to_chip_byte(), 0);
    assert_eq!(dB(3.0).to_chip_byte(), 3);
    assert_eq!(dB(-3.5).to_chip_byte(), -4);
    assert_eq!(dB(2.4).to_chip_byte(), 2);
    assert_eq!(dB(2.5).to_chip_byte(), 3);
    assert_eq!(dB(-200.0).to_chip_byte(), -90);
    assert_eq!(dB(200.0).to_chip_byte(), 90);
}
