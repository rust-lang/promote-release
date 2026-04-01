use super::*;

// Verify that the simple hyper server works.
#[test]
fn basic() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("foo"), "bar").unwrap();
    let tester = SmokeTester::new(&[dir.path().to_owned()]).unwrap();

    let addr = tester.server_addr();

    let mut response = vec![];
    let mut handle = curl::easy::Easy::new();
    handle.url(&format!("http://{addr}/foo")).unwrap();
    {
        let mut transfer = handle.transfer();
        transfer
            .write_function(|new_data| {
                response.extend_from_slice(new_data);
                Ok(new_data.len())
            })
            .unwrap();
        transfer.perform().unwrap();
    }
    assert_eq!(String::from_utf8(response).unwrap(), "bar");
}
