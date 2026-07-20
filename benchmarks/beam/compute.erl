-module(compute).
-export([main/0]).

main() ->
  L0 = erlang:monotonic_time(microsecond),
  loop(200000000),
  L1 = erlang:monotonic_time(microsecond),
  io:format("loop_ms ~p~n", [(L1 - L0) div 1000]),
  F0 = erlang:monotonic_time(microsecond),
  R = fib(35),
  F1 = erlang:monotonic_time(microsecond),
  io:format("recursion_ms ~p result ~p~n", [(F1 - F0) div 1000, R]),
  Subject = <<"beam threads this binary through two hundred million frames">>,
  T0 = erlang:monotonic_time(microsecond),
  S = tail_scan(Subject, 0, 200000000, 0),
  T1 = erlang:monotonic_time(microsecond),
  io:format("tail_scan_ms ~p result ~p~n", [(T1 - T0) div 1000, S]).

loop(0) -> ok;
loop(N) -> loop(N - 1).

fib(N) when N =< 1 -> N;
fib(N) -> fib(N - 1) + fib(N - 2).

%% Mirrors koja/tail_scan.kojs: a tail-recursive counting loop that
%% threads a heap-allocated subject through every iteration.
tail_scan(_Subject, N, N, Acc) -> Acc;
tail_scan(Subject, I, N, Acc) ->
  case I rem 13 of
    0 -> tail_scan(Subject, I + 1, N, Acc + 2);
    _ -> tail_scan(Subject, I + 1, N, Acc + 1)
  end.
