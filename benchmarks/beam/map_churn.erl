-module(map_churn).
-export([main/0]).

%% Mirrors koja/map_churn.kojs with the built-in maps. Two controls
%% come first, 1M inserts and 5M lookups that hit. Then 2M keys pass
%% through a map that holds 16 of them at a time, and 20M lookups miss
%% in that map. The last loop holds 4096 live keys.
main() ->
  P0 = erlang:monotonic_time(microsecond),
  Grown = fill(#{}, 0, 1000000),
  P1 = erlang:monotonic_time(microsecond),
  io:format("map_put_ms ~p length ~p~n", [(P1 - P0) div 1000, maps:size(Grown)]),
  G0 = erlang:monotonic_time(microsecond),
  Hits = hit(Grown, 0, 5000000, 0),
  G1 = erlang:monotonic_time(microsecond),
  io:format("map_get_ms ~p sum ~p~n", [(G1 - G0) div 1000, Hits]),
  C0 = erlang:monotonic_time(microsecond),
  Window = churn(#{}, 0, 2000000, 16),
  C1 = erlang:monotonic_time(microsecond),
  io:format("map_churn_ms ~p length ~p~n", [(C1 - C0) div 1000, maps:size(Window)]),
  M0 = erlang:monotonic_time(microsecond),
  Misses = miss(Window, 0, 20000000, 0),
  M1 = erlang:monotonic_time(microsecond),
  io:format("map_miss_ms ~p sum ~p~n", [(M1 - M0) div 1000, Misses]),
  W0 = erlang:monotonic_time(microsecond),
  Wide = churn(#{}, 0, 60000, 4096),
  W1 = erlang:monotonic_time(microsecond),
  io:format("map_wide_ms ~p length ~p~n", [(W1 - W0) div 1000, maps:size(Wide)]).

fill(Map, N, N) -> Map;
fill(Map, I, N) -> fill(maps:put(I, I, Map), I + 1, N).

hit(_Map, N, N, Sum) -> Sum;
hit(Map, I, N, Sum) -> hit(Map, I + 1, N, Sum + maps:get(I rem 1000000, Map, 0)).

%% Put key I, then remove the key that is Live places behind it.
churn(Map, N, N, _Live) -> Map;
churn(Map, I, N, Live) when I >= Live ->
  churn(maps:remove(I - Live, maps:put(I, I, Map)), I + 1, N, Live);
churn(Map, I, N, Live) ->
  churn(maps:put(I, I, Map), I + 1, N, Live).

%% Every key from 2M up is one that the map never held.
miss(_Map, N, N, Sum) -> Sum;
miss(Map, I, N, Sum) -> miss(Map, I + 1, N, Sum + maps:get(I + 2000000, Map, 1)).
