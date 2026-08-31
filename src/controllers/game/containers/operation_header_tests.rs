use super::*;

#[test]
fn malformed_explicit_operation_identity_is_not_silently_replaced() {
    let mut headers = HeaderMap::new();
    headers.insert("x-rsctf-operation-id", "not-a-uuid".parse().unwrap());
    assert!(operations::operation_request(&headers).is_err());
    headers.insert(
        "x-rsctf-operation-id",
        Uuid::nil().to_string().parse().unwrap(),
    );
    assert!(operations::operation_request(&headers).is_err());
    headers.remove("x-rsctf-operation-id");
    assert!(operations::operation_request(&headers).is_ok());
}
