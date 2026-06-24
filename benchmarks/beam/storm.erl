-module(storm).
-export([main/0]).

main() ->
  Procs = 10000,
  Iters = 50000,
  T0 = erlang:monotonic_time(microsecond),
  Self = self(),
  [spawn(fun() -> Self ! {done, busy(0, Iters, 0)} end) || _ <- lists:seq(1, Procs)],
  Total = collect(Procs, 0),
  T1 = erlang:monotonic_time(microsecond),
  io:format("storm_ms ~p procs ~p total ~p~n", [(T1 - T0) div 1000, Procs, Total]).

busy(I, N, Acc) when I >= N -> Acc;
busy(I, N, Acc) -> busy(I + 1, N, Acc + I * 7 + 3).

collect(0, Acc) -> Acc;
collect(N, Acc) -> receive {done, V} -> collect(N - 1, Acc + V) end.
