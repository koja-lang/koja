-module(strings).
-export([main/0]).

%% Mirrors koja/string_build.kojs: one process appends 10M pieces to a
%% single binary accumulator, through a plain append and through an
%% append of a formatted integer. Both loops hit the binary append
%% optimization, so the accumulator grows in place.
main() ->
  N = 10000000,
  C0 = erlang:monotonic_time(microsecond),
  S = concat(<<>>, 0, N),
  C1 = erlang:monotonic_time(microsecond),
  io:format("concat_ms ~p bytes ~p~n", [(C1 - C0) div 1000, byte_size(S)]),
  I0 = erlang:monotonic_time(microsecond),
  A = interp(<<>>, 0, N),
  I1 = erlang:monotonic_time(microsecond),
  io:format("interp_ms ~p bytes ~p~n", [(I1 - I0) div 1000, byte_size(A)]).

concat(Acc, N, N) -> Acc;
concat(Acc, I, N) -> concat(<<Acc/binary, "piece">>, I + 1, N).

interp(Acc, N, N) -> Acc;
interp(Acc, I, N) ->
  Digit = integer_to_binary(I rem 10),
  interp(<<Acc/binary, Digit/binary>>, I + 1, N).
