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
  io:format("recursion_ms ~p result ~p~n", [(F1 - F0) div 1000, R]).

loop(0) -> ok;
loop(N) -> loop(N - 1).

fib(N) when N =< 1 -> N;
fib(N) -> fib(N - 1) + fib(N - 2).
