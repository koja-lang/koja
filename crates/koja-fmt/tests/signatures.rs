mod common;

use common::*;
use koja_ast::util::dedent;
use koja_fmt::format_signature;
use koja_parser::ParseMode;

#[test]
fn error_channel_signature_stays_inline() {
    assert_unchanged(
        "
            fn parse(s: String) -> Int ! ParseError
              1
            end
        ",
    );
}

#[test]
fn union_error_signature_stays_inline() {
    assert_unchanged(
        "
            fn fetch(url: String) -> Limits ! HTTP.Error | ParseError
              parse(url)
            end
        ",
    );
}

#[test]
fn protocol_method_error_signature_round_trips() {
    assert_unchanged(
        "
            protocol Decode
              fn decode(self, raw: Binary) -> Self ! DecodeError
            end
        ",
    );
}

#[test]
fn bare_error_signature_round_trips() {
    assert_unchanged(
        "
            fn ping(host: String) ! String
              send(host)
            end

            protocol Store
              fn flush(self) ! StoreError
            end
        ",
    );
}

#[test]
fn unit_error_signature_normalizes_to_bare_form() {
    assert_fmt(
        "
            fn ping(host: String) -> () ! String
              send(host)
            end
        ",
        "
            fn ping(host: String) ! String
              send(host)
            end
        ",
    );
}

#[test]
fn protocol_method_signature_wraps_at_width() {
    // Protocol method signatures share the function signature
    // renderer and wrap the same way.
    assert_fmt(
        "
            protocol Transport
              fn send_datagram_with_options(self, payload: Binary, destination_address: String, destination_port: Int32, ttl: Int32) -> Int ! SendError
            end
        ",
        "
            protocol Transport
              fn send_datagram_with_options(
                self,
                payload: Binary,
                destination_address: String,
                destination_port: Int32,
                ttl: Int32,
              ) -> Int ! SendError
            end
        ",
    );
}

#[test]
fn long_union_alias_packs_with_trailing_pipe() {
    // Unions pack like symbolic operator chains.
    assert_fmt(
        "
            type IncomingNetworkEvent = ConnectionEstablished | ConnectionClosed | DataReceived | HandshakeTimeout | ProtocolViolation
        ",
        "
            type IncomingNetworkEvent = ConnectionEstablished | ConnectionClosed |
              DataReceived | HandshakeTimeout | ProtocolViolation
        ",
    );
}

#[test]
fn generic_params_with_bounds_wrap_like_parens() {
    // The angle-bracket list breaks one entry per line, and an
    // entry keeps its bounds intact.
    assert_fmt(
        "
            fn merge_sorted<TElement: Comparable & Hash & Equality, TCollection: Iterable & Equality>(left: TCollection, right: TCollection) -> TCollection
              left
            end
        ",
        "
            fn merge_sorted<
              TElement: Comparable & Hash & Equality,
              TCollection: Iterable & Equality
            >(left: TCollection, right: TCollection) -> TCollection

              left
            end
        ",
    );
}

#[test]
fn short_fallible_return_tail_groups_on_continuation() {
    assert_fmt(
        "
            priv fn parse_embedded_v4(text: String, allowed: Bool, address: String) -> List<Int> ! IPAddress.ParseError
              []
            end
        ",
        "
            priv fn parse_embedded_v4(text: String, allowed: Bool, address: String)
              -> List<Int> ! IPAddress.ParseError

              []
            end
        ",
    );
}

#[test]
fn protocol_fallible_return_tail_groups_on_continuation() {
    assert_fmt(
        "
            protocol Decoder
              fn decode_packet_with_address(self, packet: Binary, address: String) -> Int ! ParseError
            end
        ",
        "
            protocol Decoder
              fn decode_packet_with_address(self, packet: Binary, address: String)
                -> Int ! ParseError
            end
        ",
    );
}

#[test]
fn union_and_bare_error_tails_group_on_continuation() {
    assert_fmt(
        "
            fn fetch_resource_with_limits(url: String, retries: Int) -> Limits ! HTTP.Error | ParseError
              fetch(url)
            end

            fn flush_remote_replica_with_timeout(replica: Replica, timeout: Duration) ! StoreError
              flush(replica)
            end
        ",
        "
            fn fetch_resource_with_limits(url: String, retries: Int)
              -> Limits ! HTTP.Error | ParseError

              fetch(url)
            end

            fn flush_remote_replica_with_timeout(replica: Replica, timeout: Duration)
              ! StoreError

              flush(replica)
            end
        ",
    );
}

#[test]
fn long_fallible_return_tail_splits_as_last_resort() {
    // A tail that cannot fit on its continuation line separates the
    // success and error clauses.
    assert_fmt(
        "
            fn parse_configuration_file(path: String) -> ConfigurationDocumentWithExtendedMetadata ! ConfigurationParseOrValidationError
              1
            end
        ",
        "
            fn parse_configuration_file(path: String)
              -> ConfigurationDocumentWithExtendedMetadata
              ! ConfigurationParseOrValidationError

              1
            end
        ",
    );
}

// format_signature renders a header alone, the way an editor hover
// shows it.

fn first_function(source: &str) -> koja_ast::ast::Function {
    let result = koja_parser::parse(&koja_ast::util::dedent(source), ParseMode::File);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    match result.ast.items.into_iter().next() {
        Some(koja_ast::ast::Item::Function(f)) => f,
        other => panic!("expected a function, got {other:?}"),
    }
}

#[test]
fn format_signature_keeps_a_short_header_on_one_line() {
    let f = first_function(
        "
            fn add(a: Int, b: Int) -> Int
              a + b
            end
        ",
    );
    assert_eq!(
        format_signature(&f, "add", 80),
        "fn add(a: Int, b: Int) -> Int"
    );
}

#[test]
fn format_signature_breaks_a_long_header_one_parameter_per_line() {
    let f = first_function(
        "
            priv fn prepare_and_run(self, key: String, sql: String, oids: List<Int>, texts: List<Option<String>>, stale_close: Binary) -> (Connection, Result<QueryResult, Error>)
            end
        ",
    );
    assert_eq!(
        format_signature(&f, "Connection.prepare_and_run", 80),
        dedent(
            "
            priv fn Connection.prepare_and_run(
              self,
              key: String,
              sql: String,
              oids: List<Int>,
              texts: List<Option<String>>,
              stale_close: Binary,
            ) -> (Connection, Result<QueryResult, Error>)"
        )
        .trim_start()
    );
}

#[test]
fn format_signature_keeps_bounds_and_the_error_tail() {
    let f = first_function(
        "
            fn load<T: Decode>(path: String) -> T ! IOError
            end
        ",
    );
    assert_eq!(
        format_signature(&f, "load", 80),
        "fn load<T: Decode>(path: String) -> T ! IOError"
    );
}
