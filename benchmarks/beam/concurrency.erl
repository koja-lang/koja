-module(concurrency).
-export([main/0]).

main() ->
  Server = spawn(fun() -> server() end),
  T0 = erlang:monotonic_time(microsecond),
  msg_loop(Server, 1000000),
  T1 = erlang:monotonic_time(microsecond),
  io:format("msg_ms ~p~n", [(T1 - T0) div 1000]),
  S0 = erlang:monotonic_time(microsecond),
  spawn_loop(100000),
  S1 = erlang:monotonic_time(microsecond),
  io:format("spawn_ms ~p~n", [(S1 - S0) div 1000]).

server() -> receive {From, ping} -> From ! pong, server() end.

msg_loop(_S, 0) -> ok;
msg_loop(S, N) -> S ! {self(), ping}, receive pong -> ok end, msg_loop(S, N - 1).

spawn_loop(0) -> ok;
spawn_loop(N) ->
  Self = self(),
  P = spawn(fun() -> server() end),
  P ! {Self, ping},
  receive pong -> ok end,
  spawn_loop(N - 1).
