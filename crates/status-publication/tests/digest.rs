use status_publication::digest;

#[test]
fn digest_preserves_lowercase_zero_padded_sha256() {
    assert_eq!(
        digest(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
