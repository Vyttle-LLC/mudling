# The prototype's tests in Rust

Every test in `mud-client-spike/Tests/MUDKitTests`, and where it went. The prototype counted 278: 86 are ported, 87 were already covered by a Rust test that checks the same behaviour, and 105 are not carried over, each with the reason. The directory holds 22 Swift files and 5 fixtures, not the 23 files the ticket counted; the test count holds.

A parameterized Swift test counts once. A row can share a Rust test with its neighbors when one test checks both.

## Where the Rust tests are

| Name starts with | File |
|---|---|
| `core::` | `crates/mudling-core/tests/prototype/`, by module: `core::map::x` is `map.rs` |
| `avatar::` | `plugins/avatar/tests/prototype/`, by module |
| `core-it::` | `crates/mudling-core/tests/`: `core-it::defaults::x` is `defaults.rs` |
| `avatar-it::` | `plugins/avatar/tests/`, the same way |
| `unit::` | Unit tests in `crates/mudling-core/src/`: `unit::map::x` is in `map/tests.rs`, `unit::persist::x` in `persist/tests.rs`, `unit::ansi::x` in `ansi.rs` |

The five walks in `plugins/avatar/tests/fixtures/walks/` are cut from the prototype's fixtures, 166 KB in all, to 12 KB. Each is the shortest stretch whose test still passes: loops were taken out, and some stretches joined by a jump with no move, as a recall does. Rooms, zones and descriptions are renamed, keeping which are the same. Only the area and room the plugin's maze rule names keep their real names. No test needs a long real walk or the local logs. The whole walks stay in the prototype's repo, and the parity check (VYT-188) replays real logs.

The cuts make these tests narrower than the prototype's:

| Walk | Moves | Kept |
|---|---|---|
| `meadow-walk.txt` | 13 of 61 | Around the tree, through it, and the wagon's loop |
| `tree-portals-walk.txt` | 3 of 44 | The portal and the tunnel; none of the other areas' rooms |
| `meadow-lookalikes-walk.txt` | 6 of 97 | The three rooms told apart by exits |
| `narrow-trail-walk.txt` | 15 of 154 | The corridor, walked twice; rebuilt from its real arrivals to keep the second walk-in |
| `brothers-grimm-walk.txt` | 11 of 142 | The home's six ways and the maze; each story is a room or two |

The walks end in `.txt` because the repo keeps session logs out with `*.log`.

## Bugs the ports found

All four are fixed, each with its test as proof.

1. **A long walk sent one line over and over.** `25n` went out as 25 identical `north` lines, and Merc-derived games disconnect a player for that. A repeated move now alternates its spelling: `CommandVariety` in `defaults/commands.rs`.
2. **Blank lines piled up.** Hiding a prompt left the blank lines around it, so runs of them showed in the chat. A run now shows as one, and a blank line before anything else doesn't show.
3. **A command could land before the text it answered.** Text the server hadn't ended, such as a prompt, waited for silence, so a quick command showed above it. Sending now shows that text first, in the chat only: the line stays pending, so the plugins, the map and the prompt learner still read it whole.
4. **A character with no saved password got a blank one.** Auto-login answered the password prompt with an empty line. With no password saved it now stops after the name, and the player types the rest.

## Gaps reported, in areas this ticket doesn't own

- **Avatar can't auto-login.** Its plugin has no Connection point, so it names no login prompts and credentials do nothing. The prototype knew them. Ported on a made-up game in `core::login`.
- **Doors don't update in place.** The prototype refreshed a room's exits from `Char.Status`, so opening a door showed at once. Nothing in `map/` does that between Room.Infos, so a route still says "open" for a door already open.
- **Marking a mob by a line with no "is" or "are" marks the wrong word.** `MobMemory::set_aggressive` takes the last word of "A myconoid shaman sways here, spores drifting.", which is "drifting". The prototype took the noun by `TargetKeyword`, which is VYT-249's.
- **The `[[alias]]` Commands point is declared but never run.** VYT-212 brings aliases.

## Differences that are intended

- **Avatar has no chain separator.** The prototype chained with `=`. The spam check counts each command in a chain, so a chain saves nothing, and a refused move would take the rest with it. The player chains with `;` on every game, and the map hears each command. `n=n=w` typed by hand goes to the game as it is, so the map doesn't follow it.
- **MCCP2 is taken up.** The prototype refused it; Phase 1 added it.
- **Truecolor is dropped.** `38;2;r;g;b` is skipped cleanly, and the theme draws from its palette.
- **Empty equipment slots are kept.** The Gear panel shows them in slot order, so `equipment` has a row with an empty item for each.
- **Carried gear is asked for, not guessed.** The prototype edited its inventory from wear messages. Avatar's plugin asks for `Char.Items.Inv` again.
- **Another character's skills can't land in the list.** Stores are per character, so the prototype's replacing of the whole list isn't needed when credentials name the character. A session logged in by hand shares one unnamed character, and a skill no longer listed stays in its store until VYT-219 reads the table as a whole.


## AliasTests.swift

5 tests: 0 ported, 0 already covered, 5 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `argumentsFillTheirPlaces` | Not carried over | — | VYT-212: nothing runs aliases yet; these are its `$1`, `$*` and on/off rules. |
| `withoutPlacesArgumentsGoOnTheEnd` | Not carried over | — | VYT-212: nothing runs aliases yet; these are its `$1`, `$*` and on/off rules. |
| `missingArgumentsLeaveNoGaps` | Not carried over | — | VYT-212: nothing runs aliases yet; these are its `$1`, `$*` and on/off rules. |
| `onlyTheFirstWordCounts` | Not carried over | — | VYT-212: nothing runs aliases yet; these are its `$1`, `$*` and on/off rules. |
| `offOrUnfinishedAliasesAreSkipped` | Not carried over | — | VYT-212: nothing runs aliases yet; these are its `$1`, `$*` and on/off rules. |

## AutoMapTests.swift

77 tests: 23 ported, 39 already covered, 15 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `firstRoomStartsTheMapAndAnnouncesItsArea` | Already covered | `unit::map::the_first_room_starts_the_map_and_announces_its_area` |  |
| `movingPlacesTheNextRoomAndLinksBothWays` | Already covered | `unit::map::moving_places_the_next_room_and_links_both_ways` |  |
| `walkingBackReusesTheRoom` | Already covered | `unit::map::walking_back_and_looking_reuse_rooms` |  |
| `lookingAroundDoesNotCreateRooms` | Already covered | `unit::map::walking_back_and_looking_reuse_rooms` |  |
| `aFailedMoveDoesNotDriftTheMap` | Already covered | `unit::map::a_failed_move_does_not_drift_the_map` |  |
| `staleMoveExpectationsExpire` | Already covered | `unit::map::stale_moves_expire_but_slow_answers_count` |  |
| `aLongSpeedwalkStaysOnTheMapThoughItTakesAWhile` | Already covered | `unit::map::a_forty_move_speedwalk_answered_once_a_second_stays_connected` |  |
| `recallJumpsToAKnownRoom` | Already covered | `unit::map::a_recall_jumps_to_a_known_room` |  |
| `upAndDownChangeLevel` | Ported | `core::map::up_and_down_change_level` |  |
| `crossingIntoAnotherAreaIsReported` | Already covered | `unit::map::crossing_into_another_area_is_announced` |  |
| `occupiedCellsPushNewRoomsFurtherOut` | Ported | `core::map::a_taken_square_pushes_a_new_room_further_out` |  |
| `aWayBackThatLeadsElsewhereIsAMaze` | Already covered | `unit::map::a_way_back_that_leads_elsewhere_is_tangled` |  |
| `notesRoomFeatures` | Ported | `core::map::a_rooms_features_are_noted` |  |
| `revisitWithDifferentExitsIsFlagged` | Already covered | `unit::map::a_revisit_with_different_exits_is_flagged` |  |
| `doorStateUpdatesInPlace` | Not carried over | — | Not built: nothing updates a room's exits between Room.Infos. See gaps reported; VYT-249 owns reading `Char.Status`. |
| `survivesSaveAndLoad` | Already covered | `unit::map::the_map_survives_the_database_and_carries_on` |  |
| `MapSearchTests.findsAMobByAnyOfItsWords` | Not carried over | — | VYT-215: the Find panel. |
| `MapSearchTests.nearestResultsComeFirst` | Not carried over | — | VYT-215: the Find panel. |
| `MapSearchTests.findsRoomsByName` | Not carried over | — | VYT-215: the Find panel. |
| `MapSearchTests.blankQueryFindsNothing` | Not carried over | — | VYT-215: the Find panel. |
| `MapSearchTests.routesFollowExploredLinks` | Already covered | `unit::map::a_route_follows_explored_links_and_opens_closed_doors` |  |
| `MapSearchTests.walkingOpensClosedDoors` | Already covered | `unit::map::a_route_follows_explored_links_and_opens_closed_doors` |  |
| `MapSearchTests.queuedMovesMapInOrder` | Already covered | `unit::map::queued_moves_map_in_order` |  |
| `MapSearchTests.roomsSavedBeforeSightingsStillLoad` | Not carried over | — | VYT-194: reading the prototype's saved files. |
| `RoomAnnotationTests.notesAndMarkersCanBeSetOnAnyRoom` | Ported | `core::map::notes_and_marks_can_be_set_on_any_room` |  |
| `RoomAnnotationTests.notesAreSearchable` | Not carried over | — | VYT-215: the Find panel. |
| `RoomAnnotationTests.roomsSavedBeforeNotesStillLoad` | Not carried over | — | VYT-194: reading the prototype's saved files. |
| `LookAlikeRoomTests.aSquareOfIdenticalRoomsMapsCleanly` | Ported | `core::map::a_square_of_identical_rooms_maps_cleanly` |  |
| `LookAlikeRoomTests.aLookAmongLookAlikesStaysPut` | Ported | `core::map::a_look_among_look_alikes_stays_put` |  |
| `LookAlikeRoomTests.anUnexplainedRoomInfoAmongLookAlikesStaysPut` | Ported | `core::map::an_unexplained_room_among_look_alikes_stays_put` |  |
| `LookAlikeRoomTests.aMoveTheRoomCouldNotMakeIsIgnored` | Already covered | `unit::map::a_move_the_room_could_not_make_is_ignored` |  |
| `LookAlikeRoomTests.aRefusedMoveDropsOnlyItself` | Already covered | `unit::map::a_refused_move_drops_only_itself` |  |
| `LookAlikeRoomTests.slowAnswersDuringCombatLagStillCount` | Already covered | `unit::map::stale_moves_expire_but_slow_answers_count` |  |
| `MapRepairTests.forgettingAnAreaRemovesItsRoomsAndLinks` | Already covered | `unit::map::forgetting_rooms_removes_them_and_links_to_them` |  |
| `MapRepairTests.forgettingYourRoomRebuildsFromTheSameSpot` | Already covered | `unit::map::forgetting_your_room_rebuilds_from_the_same_spot` |  |
| `MapRepairTests.imHereMovesThePlayerMarker` | Ported | `core::map::im_here_moves_the_player` |  |
| `MapRepairTests.notAMazeClearsTheMark` | Ported | `core::map::not_a_maze_clears_the_mark` |  |
| `MapBrowsingTests.followsAnExploredStairwayToItsRoom` | Not carried over | — | VYT-214: the map view browses levels. |
| `MapBrowsingTests.anUnexploredStairwayPointsOneLevelOver` | Not carried over | — | VYT-214: the map view browses levels. |
| `ManualAggroTests.markingAMobFlagsItsWholeKind` | Ported | `core::map::marking_a_mob_flags_its_whole_kind` | A room line with no "is" or "are" isn't read yet; see gaps reported. |
| `ManualAggroTests.unmarkingUndoesALearnedGuess` | Already covered | `unit::map::unmarking_undoes_a_learned_guess` |  |
| `DirectionCommandTests.parsesMovement` | Ported | `core::map::moves_read_in_any_spelling` |  |
| `DirectionCommandTests.otherCommandsAreNotMoves` | Ported | `core::map::moves_read_in_any_spelling` |  |
| `TerrainTests.classifiesEnvironments` | Not carried over | — | VYT-214: terrain is how the map view draws a room. The core keeps the game's word in `MapRoom::environment`. |
| `MobMemoryTests.learnsFromUnprovokedAttacks` | Ported | `avatar::messages::a_mob_attacking_unprovoked_marks_its_kind`<br>`core::map::a_mob_learned_from_its_attack_flags_its_kind_only` |  |
| `MobMemoryTests.aMobAssistingInAFightIsNotAggro` | Ported | `avatar::messages::a_mob_helping_a_friend_or_ordinary_combat_marks_nothing` |  |
| `MobMemoryTests.reportsEachMobOnce` | Ported | `core::map::a_mob_learned_from_its_attack_flags_its_kind_only` |  |
| `MobMemoryTests.ignoresOrdinaryCombat` | Ported | `avatar::messages::a_mob_helping_a_friend_or_ordinary_combat_marks_nothing` |  |
| `AreaNameTests.parsesAvatarZones` | Already covered | `avatar-it::world::a_zone_reads_as_its_area_and_level_range` |  |
| `AreaNameTests.singleSpacedBuilderStaysInTheName` | Already covered | `avatar-it::world::area_names_drop_the_level_range_and_builder` |  |
| `AreaNameTests.unknownFormatsPassThrough` | Already covered | `unit::session::map_rules::a_plugins_points_become_map_rules` |  |
| `DescribedRoomTests.anOffGridReturnReusesTheDescribedRoom` | Already covered | `unit::map::an_off_grid_return_reuses_the_described_room` |  |
| `DescribedRoomTests.anUnexplainedArrivalFindsTheDescribedRoom` | Ported | `core::map::an_unexplained_arrival_finds_the_described_room` |  |
| `DescribedRoomTests.roomsMappedBeforeDescriptionsLearnThem` | Ported | `core::map::rooms_mapped_before_their_description_learn_it` |  |
| `DescribedRoomTests.imHereRemovesTheStrayRoomTheMapInvented` | Already covered | `unit::map::im_here_removes_the_stray_room_the_map_invented` |  |
| `DescribedRoomTests.imHereKeepsAStrayWithNotes` | Ported | `core::map::im_here_keeps_a_stray_with_notes` |  |
| `DescribedRoomTests.identicalHallsAroundAelmonsSanctuaryStayApart` | Ported | `avatar::walks::identical_halls_around_a_sanctuary_stay_apart` |  |
| `GuessAndConfirmTests.theMeadowMapsAsTwoSeparateShades` | Already covered | `unit::map::identical_rooms_map_apart` |  |
| `GuessAndConfirmTests.lostAmongLookAlikesTheNextRoomDecides` | Already covered | `unit::map::lost_among_look_alikes_the_next_room_decides` |  |
| `GuessAndConfirmTests.aGuessIsRethoughtTwoRoomsLater` | Already covered | `unit::map::a_guess_is_rethought_two_rooms_later_and_written_once_sure` |  |
| `GuessAndConfirmTests.mobsSeenWhileUnsureStayWithTheRightRoom` | Already covered | `unit::map::mobs_seen_while_unsure_stay_with_the_right_room` |  |
| `GuessAndConfirmTests.editingTheMapSettlesOnWhatItShows` | Already covered | `unit::map::editing_the_map_settles_on_what_it_shows` |  |
| `GuessAndConfirmTests.savingWhileUnsureKeepsTheBestGuess` | Ported | `core::map::a_map_saved_while_unsure_keeps_its_best_guess` |  |
| `GuessAndConfirmTests.aCrowdedSquareIsAStretchNotAMaze` | Already covered | `unit::map::a_crowded_square_is_a_stretch_not_a_maze` |  |
| `BuildingTests.aTreeYouWalkThroughBetweenNeighborsIsLiftedOut` | Already covered | `unit::map::a_building_walked_through_between_neighbors_is_lifted_out` |  |
| `BuildingTests.floorsBelongToTheBuilding` | Ported | `core::map::floors_belong_to_the_building` |  |
| `BuildingTests.indoorRoomsThatFitAreOrdinarySquares` | Already covered | `unit::map::indoor_rooms_that_fit_are_ordinary_squares` |  |
| `BuildingTests.housesFromEarlierRulesGoBackOnTheGridWhenLoaded` | Not carried over | — | VYT-194: reading the prototype's saved files. |
| `BuildingTests.aTunnelIntoAnotherAreaIsNotAHouse` | Ported | `core::map::a_tunnel_into_another_area_is_no_house` |  |
| `BuildingTests.eachAreaHasItsOwnGrid` | Already covered | `unit::map::each_area_has_its_own_grid` |  |
| `GearMarkTests.aGearMobIsThatMobWhateverItsDoing` | Already covered | `unit::map::a_gear_mob_is_that_mob_whatever_its_doing` |  |
| `GearMarkTests.anImportantPersonIsThemWhateverTheyreDoing` | Already covered | `unit::map::an_important_person_is_them_whatever_theyre_doing` |  |
| `GearMarkTests.gearMarksSavedWithAuraTagsLoseThem` | Not carried over | — | VYT-194: reading the prototype's saved files. |
| `GearMarkTests.memorySavedBeforeGearMarksStillLoads` | Not carried over | — | VYT-194: reading the prototype's saved files. |
| `GearMarkTests.listsMarkedRoomsAndWhereGearMobsWereSeenNearestFirst` | Not carried over | — | VYT-215: the Find panel. |
| `MazeTests.lookAlikesLeadingIntoEachOtherCrookedlyAreOneMaze` | Already covered | `unit::map::hand_laid_crossroads_that_lead_crookedly_collapse` |  |
| `MazeTests.copiesOfOneCrossroadsAreNotAMaze` | Already covered | `unit::map::copies_of_one_crossroads_that_never_meet_are_not_a_maze` |  |

## CharacterRecordTests.swift

24 tests: 12 ported, 9 already covered, 3 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `CharacterParserTests.readsTheSkillListByFixedColumns` | Already covered | `avatar-it::character::slist_cuts_fixed_columns_and_reads_wrapped_rows` |  |
| `CharacterParserTests.wrappedRowsKeepTheirLevelAndPagerTextIsIgnored` | Ported | `avatar::tables::the_skill_list_leaves_out_the_pager_and_what_follows_the_prompt` |  |
| `CharacterParserTests.aFinishedListReplacesSkillsFromElsewhere` | Ported | `avatar::tables::text_after_a_list_is_no_skill` | Half: see differences for the other character's skills. |
| `CharacterParserTests.readsThePracticeTable` | Ported | `avatar::tables::the_practice_table_reads_every_cell` |  |
| `CharacterParserTests.mergesBothSources` | Ported | `avatar::tables::the_skill_list_and_practice_merge` |  |
| `CharacterParserTests.spellsAreCastAndSkillsUsedDirectly` | Not carried over | — | VYT-219: the Skills panel's actions. The table keeps each mana cost. |
| `CharacterParserTests.readsScoreExtras` | Already covered | `avatar-it::character::score_fills_the_character` |  |
| `CharacterParserTests.readsStatsFromScoreUnderGMCPKeys` | Already covered | `avatar-it::character::score_fills_the_character` |  |
| `CharacterParserTests.recordsSavedBeforeStatsStillLoad` | Not carried over | — | VYT-194: reading the prototype's saved files. |
| `CharacterParserTests.tablesSplitAcrossChunksStillParse` | Ported | `avatar::tables::a_table_split_at_any_point_still_reads` |  |
| `CharacterParserTests.ordinaryTextChangesNothing` | Ported | `avatar::tables::ordinary_text_changes_nothing` |  |
| `CharacterParserTests.practicingAtATrainerUpdatesTheSkillDirectly` | Already covered | `avatar-it::character::practicing_at_a_trainer_sets_the_skill` |  |
| `CharacterParserTests.reportsPracticeTablesAndImprovements` | Already covered | `avatar-it::character::messages_about_skills_and_affects_are_events`<br>`avatar-it::character::practice_reads_on_past_the_pager` |  |
| `CharacterParserTests.readsAffects` | Already covered | `avatar-it::character::affects_lists_each_spell_once` |  |
| `CharacterParserTests.readsAvatarsNoAffectsAnswer` | Already covered | `avatar-it::character::affects_with_nothing_on_is_an_empty_list` |  |
| `CharacterParserTests.readsEquipmentGluedToThePrompt` | Ported | `avatar::messages::equipment_glued_to_the_prompt_is_read` |  |
| `CharacterParserTests.readsEquipmentAndSkipsEmptySlots` | Already covered | `avatar-it::gear::equipment_lists_every_row_in_order_empty_slots_included` | Empty slots are kept; see differences. |
| `CharacterParserTests.reportsGearAndAffectHints` | Ported | `avatar::messages::gear_and_affect_messages_are_events` |  |
| `CharacterParserTests.gearPutOnTakesItsPlaceInTheList` | Not carried over | — | VYT-220: the Gear panel's Worn list. Between reads the store holds the last `eq`. |
| `CharacterParserTests.readsGearAfterAMarkedPrompt` | Ported | `avatar::messages::gear_after_a_marked_prompt_is_read` |  |
| `CharacterParserTests.labelsPracticeLinesAsOneTable` | Already covered | `core-it::plugin_host::a_table_runs_start_rows_and_end_and_rows_need_it_open`<br>`core-it::plugin_host::a_prompt_or_the_next_table_ends_a_table_and_output_after_a_prompt_is_read` |  |
| `CharacterParserTests.trainerMessagesAreOrdinaryOutput` | Ported | `avatar::tables::a_trainers_message_is_ordinary_output` |  |
| `CharacterParserTests.knowsWhenToPageThroughTheSkillList` | Ported | `avatar::tables::the_skill_list_pager_is_answered_too` |  |
| `CharacterParserTests.pagesThroughALongPracticeTable` | Ported | `avatar::tables::a_long_practice_table_pages_through` |  |

## CharacterRefreshPlannerTests.swift

7 tests: 0 ported, 0 already covered, 7 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `enteringTheGameReadsScoreAndPracticeAndTheSkillListOnce` | Not carried over | — | VYT-219: quiet background reads. The plugin declares these commands; nothing sends them yet. |
| `knownCharactersSkipTheSkillList` | Not carried over | — | VYT-219: quiet background reads. The plugin declares these commands; nothing sends them yet. |
| `affectAndGearChangesQueueOneReadEach` | Not carried over | — | VYT-219: quiet background reads. The plugin declares these commands; nothing sends them yet. |
| `nothingIsSentBeforeTheCharacterIsInTheGame` | Not carried over | — | VYT-219: quiet background reads. The plugin declares these commands; nothing sends them yet. |
| `aLevelUpRereadsSkills` | Not carried over | — | VYT-219: quiet background reads. The plugin declares these commands; nothing sends them yet. |
| `waitsOutFightsAndSleep` | Not carried over | — | VYT-219: quiet background reads. The plugin declares these commands; nothing sends them yet. |
| `staleSkillsRefreshWhenTheSheetOpens` | Not carried over | — | VYT-219: quiet background reads. The plugin declares these commands; nothing sends them yet. |

## CommandChainTests.swift

7 tests: 3 ported, 3 already covered, 1 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `chainsWithTheGamesSeparator` | Already covered | `core-it::defaults::a_chain_with_a_separator_is_packed_into_it` |  |
| `sendsOneCommandPerLineWithoutASeparator` | Already covered | `core-it::defaults::a_chain_without_a_separator_goes_as_separate_lines` |  |
| `keepsLinesShortEnoughForTheGame` | Already covered | `core-it::defaults::a_chain_with_a_separator_is_packed_into_it` |  |
| `aChainedLineIsEachOfItsCommands` | Ported | `core::commands::a_typed_chain_is_each_of_its_commands_to_the_map` | The player chains with `;`, whatever the game chains with; see differences. |
| `avatarChainsWithEquals` | Not carried over | — | Intended: Avatar sends one command a line now, proved by `avatar-it::world::commands_go_one_per_line_with_the_games_verbs_and_doors`. See differences. Reading old saved servers is VYT-194. |
| `repeatedMovesAlternateTheirSpelling` | Ported | `core::commands::repeated_moves_alternate_their_spelling`<br>`core::commands::a_long_walk_never_repeats_a_line`<br>`core::commands::a_move_typed_again_is_spelled_the_other_way` | Bug 1. |
| `otherCommandsGoAsTyped` | Ported | `core::commands::other_commands_go_as_typed` |  |

## CommandHistoryTests.swift

6 tests: 0 ported, 6 already covered, 0 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `upWithNothingTypedWalksBackThroughEverything` | Already covered | `unit::completion::history::up_with_nothing_typed_walks_back_through_everything` |  |
| `upFindsOnlyCommandsStartingWithWhatWasTyped` | Already covered | `unit::completion::history::up_finds_only_commands_starting_with_what_was_typed` |  |
| `downGoesBackToWhatWasBeingTyped` | Already covered | `unit::completion::history::down_goes_back_to_what_was_being_typed` |  |
| `theSentCommandStillInTheBoxIsSkipped` | Already covered | `unit::completion::history::the_sent_command_still_in_the_box_is_skipped` |  |
| `repeatsAreKeptOnceAndOldestDropFirst` | Already covered | `unit::completion::history::empty_and_repeated_commands_are_not_stored`<br>`unit::completion::history::only_the_last_500_are_kept` |  |
| `typingStartsTheSearchOver` | Already covered | `unit::completion::history::typing_starts_the_search_over` |  |

## ConnectionBookTests.swift

10 tests: 0 ported, 0 already covered, 10 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `startsWithAvatarSelected` | Not carried over | — | VYT-205: saved servers and characters. |
| `addingACharacterCapitalizesSelectsAndDedupes` | Not carried over | — | VYT-205: saved servers and characters. |
| `charactersBelongToTheirServer` | Not carried over | — | VYT-205: saved servers and characters. |
| `addingAKnownAddressReusesTheServer` | Not carried over | — | VYT-205: saved servers and characters. |
| `removingAServerTakesItsCharactersAlong` | Not carried over | — | VYT-205: saved servers and characters. |
| `forgettingTheSelectedCharacterFallsBackToLoggingInByHand` | Not carried over | — | VYT-205: saved servers and characters. |
| `survivesSaveAndLoad` | Not carried over | — | VYT-205: saved servers and characters. |
| `avatarKeepsItsFeaturesOtherGamesStartSafe` | Not carried over | — | VYT-232: plugins per server. Features became plugins, off until the player turns them on. |
| `serversSavedBeforeFeaturesGetTheirGamesDefaults` | Not carried over | — | VYT-194: reading the prototype's saved files. |
| `turnedOffFeaturesStayOff` | Not carried over | — | VYT-232: plugins per server. |

## GamePromptTests.swift

5 tests: 2 ported, 3 already covered, 0 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `learnsAnyStyleOfPrompt` | Ported | `avatar::rooms::any_style_of_prompt_is_learned` | Through the engine; the fourth style is the `<...><...>` one the plugin's prompt fragment names. |
| `onlyTextAfterVitalsIsAPrompt` | Already covered | `core-it::defaults::only_text_after_vitals_is_learned` |  |
| `theBarePromptWinsOverOneLearnedWithOutputGlued` | Already covered | `core-it::defaults::a_bare_prompt_wins_over_one_learned_with_output_glued` |  |
| `halfAPromptDoesNotHideTheWholeOne` | Ported | `avatar::rooms::half_a_prompt_does_not_hide_the_whole_one` |  |
| `leavesOtherTextAlone` | Already covered | `core-it::defaults::leaves_other_text_alone` |  |

## GameStateTests.swift

10 tests: 2 ported, 2 already covered, 6 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `roomInfoSetsRoomAndExits` | Ported | `avatar::rooms::room_info_sets_the_room_and_its_exits` |  |
| `roomPlayersStripANSIAndMarkSelf` | Not carried over | — | VYT-249: the core publishes no store for who and what is in the room. |
| `addAndRemovePlayerAdjustOccupants` | Not carried over | — | VYT-249: the core publishes no store for who and what is in the room. |
| `vitalsParseStringNumbersAndLag` | Ported | `avatar::rooms::vitals_read_numbers_sent_as_strings` | Lag and position have no store yet: VYT-249. |
| `statusDiffSetsAndClearsOpponent` | Not carried over | — | VYT-249: current target and its health. |
| `statusDiffMergesRoomAndAffects` | Not carried over | — | VYT-249: the core doesn't read `Char.Status`. Affects come from `aff`, see `avatar-it::character::affects_lists_each_spell_once`. Door state: see gaps reported. |
| `itemListsAndIncrementalUpdates` | Already covered | `unit::session::inventory::items_come_and_go_and_other_places_are_ignored` | Items carried. Items on the ground are VYT-249. |
| `statusMergesIntoACharacterSheet` | Not carried over | — | VYT-219: the sheet comes from `score`, see `avatar-it::character::score_fills_the_character`. Live `Char.Status` values are VYT-249. |
| `itemsWithRawColorCodesInTheirNamesStillArrive` | Already covered | `unit::session::inventory::raw_color_codes_in_a_name_are_read_and_stripped` |  |
| `channelListParses` | Not carried over | — | The prototype parsed `Comm.Channel.List` and never showed it. No ticket asks for it. |

## GearSetTests.swift

29 tests: 2 ported, 4 already covered, 23 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `GearPlanTests.aPieceSavedWithItsSlotMakesRoomThere` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.piecesInABagComeOutAndGoOn` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.piecesAlreadyWornAreLeftAlone` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.carriedPiecesGoStraightOn` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.piecesFoundNowhereAreReported` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.copiesCountSeparately` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.aGuessedWordPicksOutItsItemAmongWhatsNearby` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.whenASetOverfillsASlotTheLaterPieceWins` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.avoidsWordsTheGameWontTake` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.whatComesOffGoesIntoTheContainerWhileTheSetGoesOn` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.setsSavedWithABagPerPieceKeepThatBag` | Not carried over | — | VYT-194: reading the prototype's saved files. |
| `GearPlanTests.countsWhenNoWordTellsItemsApart` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.usesTheStartOfAHyphenatedWord` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.wordsGivenToAnItemNameItInEveryCommand` | Not carried over | — | VYT-221: gear sets. |
| `GearPlanTests.pastLogsGiveTheKeywordsOfItemsIdentifiedThere` | Not carried over | — | VYT-194: gear data comes over in the import; MUDling keeps no chat logs to read. |
| `GearPlanTests.setsSavedWithAScriptStillLoad` | Not carried over | — | VYT-194: reading the prototype's saved files. |
| `GearPlanTests.remembersTheSlotEachItemWasWornIn` | Already covered | `avatar-it::gear::equipment_lists_every_row_in_order_empty_slots_included` |  |
| `GearPlanTests.wearMessagesTeachTheSlotToo` | Already covered | `avatar-it::gear::wear_remove_and_put_are_events_and_wearing_notes_the_slot` |  |
| `GearPlanTests.twoNeckPiecesEachMakeRoom` | Not carried over | — | VYT-221: gear sets. |
| `PutAwayMessageTests.readsWhatWentInAndWhatDidNot` | Already covered | `avatar-it::gear::wear_remove_and_put_are_events_and_wearing_notes_the_slot` |  |
| `WornListTests.aWearFollowedByEqInTheSameTextIsCountedOnce` | Ported | `avatar::messages::a_wear_followed_by_eq_in_one_read_is_counted_once` |  |
| `WornListTests.aGearLineRightAfterAnEqTableStillCounts` | Ported | `avatar::messages::a_gear_line_right_after_an_eq_table_still_counts` |  |
| `CarriedGearTests.wearAndRemoveMessagesKeepTheInventoryCurrent` | Already covered | `avatar-it::gear::wearing_or_removing_asks_for_the_inventory_again` | MUDling asks the game instead of guessing; see differences. |
| `GearCopyTests.copiesWithIdsAreNamedByThem` | Not carried over | — | VYT-221: gear sets. |
| `GearCopyTests.theChosenCopyReplacesAnotherOfItsName` | Not carried over | — | VYT-221: gear sets. |
| `GearCopyTests.anotherOfTheNameStandsInForACopyThatsGone` | Not carried over | — | VYT-221: gear sets. |
| `GearCopyTests.anEqReadKeepsTheIdsOfWhatsStillInItsSlot` | Not carried over | — | VYT-221: gear sets. |
| `GearCopyTests.takingOffOneOfTwoByItsIdKeepsTheOtherCopyOn` | Not carried over | — | VYT-221: gear sets. |
| `GearCopyTests.wearingTheCopyMUDPadNamedTakesThatOneFromTheInventory` | Not carried over | — | VYT-221: gear sets. |

## GearTests.swift

14 tests: 0 ported, 0 already covered, 14 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `GearChangeTests.wearingIntoAnEmptySlotMeasuresTheEffects` | Not carried over | — | VYT-220: gear measured by what wearing it changes. |
| `GearChangeTests.takingOffAloneMeasuresTheReverse` | Not carried over | — | VYT-220: gear measured by what wearing it changes. |
| `GearChangeTests.aSwapMeasuresNeither` | Not carried over | — | VYT-220: gear measured by what wearing it changes. |
| `GearChangeTests.unknownBeforeValuesAreNotGuessed` | Not carried over | — | VYT-220: gear measured by what wearing it changes. |
| `GearDatabaseTests.aCopysIdentifyIsItsOwnAndTeachesTheItemsWords` | Not carried over | — | VYT-220: the gear database, per copy and per item. |
| `GearDatabaseTests.anotherCopyIsEstimatedWithoutTheEnchants` | Not carried over | — | VYT-220: the gear database, per copy and per item. |
| `GearDatabaseTests.copiesThatDifferGiveARange` | Not carried over | — | VYT-220: the gear database, per copy and per item. |
| `GearDatabaseTests.measuredStatsCountOnlyWhenNothingWasIdentified` | Not carried over | — | VYT-220: the gear database, per copy and per item. |
| `GearDatabaseTests.anIdentifyIsNeverReplacedByMeasuring` | Not carried over | — | VYT-220: the gear database, per copy and per item. |
| `GearDatabaseTests.aMeasurementOfNothingKeepsWhatWasSeen` | Not carried over | — | VYT-220: the gear database, per copy and per item. |
| `GearDatabaseTests.aMagicTagOnAPlainItemHintsAtAnEnchant` | Not carried over | — | VYT-220: the gear database, per copy and per item. |
| `GearDatabaseTests.theSameIdentifyByNameCountsOnce` | Not carried over | — | VYT-220: the gear database, per copy and per item. |
| `GearDatabaseTests.savesAndLoads` | Not carried over | — | VYT-220: the gear database, per copy and per item. |
| `GearDatabaseTests.searchFindsItemsByAnyWord` | Not carried over | — | VYT-220: the gear database, per copy and per item. |

## IdentifyTests.swift

5 tests: 1 ported, 3 already covered, 1 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `readsAShopIdentifyUpToThePrompt` | Already covered | `avatar-it::gear::identify_splits_base_from_enchant` |  |
| `theSpellGivesKeywordsAndAddsUpModifiers` | Ported | `avatar::messages::the_spell_gives_the_words_and_adds_up_what_it_names` | `details` lines have no store yet: VYT-220. |
| `enchantsAreKeptApartFromWhatTheItemDoes` | Already covered | `avatar-it::gear::identify_splits_base_from_enchant` |  |
| `pastLogsMatchTheSpellsIdentifiesToWhatWasCarried` | Not carried over | — | VYT-194: gear data comes over in the import; MUDling keeps no chat logs to read. |
| `examineGivesTheSlot` | Already covered | `avatar-it::gear::examine_notes_the_slot_an_item_goes_in` |  |

## MapHistoryTests.swift

3 tests: 1 ported, 1 already covered, 1 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `savedVersionsComeBackNewestFirstWithWhatTheyHeld` | Already covered | `unit::persist::map_versions_save_list_and_restore` |  |
| `oldAutomaticVersionsArePrunedNamedOnesKept` | Not carried over | — | VYT-216: versions are saved and listed; pruning arrives with map history. |
| `versionsSavedTheSameMomentDontOverwriteEachOther` | Ported | `core::map::map_versions_saved_the_same_moment_are_both_kept` |  |

## MapReplayTests.swift

12 tests: 11 ported, 0 already covered, 1 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `aStretchedLoopOfLookAlikesClosesOnTheStart` | Ported | `avatar::walks::a_stretched_loop_of_look_alikes_closes_on_the_start` |  |
| `darkRoomsAreNotMappedAndTheNextRoomSeenGoesWhereTheMovesLead` | Ported | `avatar::walks::dark_rooms_are_not_mapped_and_the_next_room_seen_goes_where_the_moves_lead` |  |
| `inTheDarkTheMapFollowsExitsItAlreadyKnows` | Ported | `avatar::walks::in_the_dark_the_map_follows_exits_it_already_knows` | Without the step on `refreshExits`; see gaps reported. |
| `aRefusedMoveIsDropped` | Ported | `avatar::walks::a_refused_move_is_dropped` |  |
| `aFleeIsFollowedFromTheText` | Ported | `avatar::walks::a_flee_is_followed_from_the_text` |  |
| `mapNotesRoundTripThroughTheLog` | Not carried over | — | The `[MAP]` note is the prototype's log format. What it checked, which commands are moves or looks, is `unit::map::other_commands_do_not_disturb_pending_moves`. Debug logs are VYT-229. |
| `theTreeOfKnowledgeStandsInTheMeadowLikeABuilding` | Ported | `avatar::walks::the_tree_of_knowledge_stands_in_the_meadow_like_a_building` |  |
| `theWholeMeadowIsAnExactGridWithTheTreeOnAPath` | Ported | `avatar::walks::the_meadow_is_an_exact_grid_with_the_tree_on_a_path` | `meadow-walk.txt`. |
| `portalsLeadToOtherAreasAndTunnelsArentHouses` | Ported | `avatar::walks::portals_lead_to_other_areas_and_tunnels_arent_houses` | `tree-portals-walk.txt`. |
| `lookAlikesWithTheSameDescriptionAreToldApartByTheirExits` | Ported | `avatar::walks::look_alikes_with_the_same_description_are_told_apart_by_their_exits` | `meadow-lookalikes-walk.txt`. |
| `aCorridorOfIdenticalRoomsIsARowNotOneRoom` | Ported | `avatar::walks::a_corridor_of_identical_rooms_is_a_row_not_one_room` | `narrow-trail-walk.txt`. |
| `eachStoryOfTheBrothersGrimmIsAMapOfItsOwn` | Ported | `avatar::walks::each_story_of_the_brothers_grimm_is_a_map_of_its_own` | `brothers-grimm-walk.txt`. |

## MonitorAndAlignmentTests.swift

3 tests: 0 ported, 0 already covered, 3 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `MonitorTests.monitorHealthComesAndGoesWithGMCP` | Not carried over | — | VYT-207: targeting and monitoring. |
| `MonitorTests.monitorTargetFromCommand` | Not carried over | — | VYT-207: targeting and monitoring. |
| `AlignmentTests.wordsMatchAvatarsScore` | Not carried over | — | VYT-219: the Character panel shows it. It's Avatar's wording, so it goes in the plugin then. |

## OutputRouterTests.swift

8 tests: 1 ported, 2 already covered, 5 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `LineSplitterTests.holdsTheUnfinishedTail` | Already covered | `unit::line::a_prompt_without_a_newline_waits_for_more_text`<br>`unit::line::a_split_at_any_byte_changes_nothing` |  |
| `LineSplitterTests.keepsColorCodes` | Already covered | `unit::line::styles_are_spans_of_the_plain_text` |  |
| `passesEverythingThroughWhenNothingIsPending` | Ported | `avatar::tables::a_table_the_player_asked_for_shows_and_is_read` |  |
| `hidesTheTablesItAskedFor` | Not carried over | — | VYT-219: quiet background reads. The Character panel's reads send and hide these tables; nothing does yet. |
| `thePlayersOwnTableAfterTheQuietOneShows` | Not carried over | — | VYT-219: quiet background reads. The Character panel's reads send and hide these tables; nothing does yet. |
| `chatterAndStrayPromptsDontConfuseIt` | Not carried over | — | VYT-219: quiet background reads. The Character panel's reads send and hide these tables; nothing does yet. |
| `aLineStartedBeforeQuietIsNotRepeated` | Not carried over | — | VYT-219: quiet background reads. The Character panel's reads send and hide these tables; nothing does yet. |
| `aTimeoutReleasesTheHeldLineAndForgetsThePendingRead` | Not carried over | — | VYT-219: quiet background reads. The Character panel's reads send and hide these tables; nothing does yet. |

## RoomTextTrackerTests.swift

5 tests: 5 ported, 0 already covered, 0 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `readsTheFirstDescriptionLineAfterTheExits` | Ported | `avatar::rooms::the_description_is_the_first_line_after_the_exits` |  |
| `readsTheNameWhenThePromptHadItsOwnLine` | Ported | `avatar::rooms::the_name_is_read_when_the_prompt_had_its_own_line` |  |
| `anotherRoomsTextGivesNothing` | Ported | `avatar::rooms::another_rooms_text_gives_no_description` |  |
| `eachDescriptionIsTakenOnce` | Ported | `avatar::rooms::a_description_is_taken_once` |  |
| `readsTheDescriptionWhenTheNameCarriesATag` | Ported | `avatar::rooms::the_description_is_read_when_the_name_carries_a_tag` |  |

## SpeedwalkTests.swift

3 tests: 1 ported, 2 already covered, 0 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `expandsCountsAndDoors` | Already covered | `core-it::defaults::speedwalk_expands_to_moves` |  |
| `spacesWorkAsWellAsCommas` | Already covered | `core-it::defaults::speedwalk_expands_to_moves` |  |
| `otherCommandsGoAsTyped` | Ported | `core::commands::what_isnt_a_walk_goes_as_typed` |  |

## TargetKeywordTests.swift

3 tests: 1 ported, 0 already covered, 2 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `occupantKeyword` | Not carried over | — | VYT-249: clean target keywords for the room's occupants. |
| `playersAreTargetedByName` | Not carried over | — | VYT-249: clean target keywords for the room's occupants. |
| `itemKeyword` | Ported | `avatar::messages::an_items_word_is_the_noun_of_its_name` |  |

## TelnetParserTests.swift

19 tests: 17 ported, 2 already covered, 0 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `plainTextPassesThrough` | Already covered | `unit::telnet::text_is_sliced_from_the_chunk_without_copying` |  |
| `escapedIACBecomesLiteralByte` | Ported | `core::telnet::an_escaped_iac_is_a_literal_byte` |  |
| `negotiationEmitsAfterPrecedingText` | Ported | `core::telnet::a_negotiation_comes_after_the_text_before_it` |  |
| `gmcpSubnegotiationParses` | Ported | `core::telnet::a_gmcp_message_is_read_whole_however_it_arrives` |  |
| `sequencesSplitAcrossChunksSurvive` | Ported | `core::telnet::a_gmcp_message_is_read_whole_however_it_arrives` |  |
| `escapedIACInsideSubnegotiationUnescapes` | Ported | `core::telnet::an_escaped_iac_inside_a_message_is_unescaped` |  |
| `goAheadFlushesText` | Ported | `core::telnet::go_ahead_ends_the_text_before_it` |  |
| `TelnetNegotiatorTests.acceptsGMCPAndSetsFlag` | Ported | `core::telnet::a_negotiation_comes_after_the_text_before_it` |  |
| `TelnetNegotiatorTests.refusesOtherOffersAndRequests` | Ported | `core::telnet::offers_are_taken_up_or_refused` | MCCP2 is now taken up, on purpose; see differences. |
| `TelnetNegotiatorTests.acknowledgementsNeedNoReply` | Ported | `core::telnet::acknowledgements_need_no_reply` |  |
| `GMCPTests.messageWithPayloadParses` | Already covered | `unit::telnet::gmcp_round_trips_through_the_parser` |  |
| `GMCPTests.messageWithoutPayloadParses` | Ported | `core::telnet::a_message_without_a_payload_has_no_data` |  |
| `GMCPTests.decodedPayloadIsJSON` | Ported | `core::telnet::a_payload_is_json` |  |
| `GMCPTests.encodeWrapsInGMCPFrame` | Ported | `core::telnet::a_message_goes_out_in_a_gmcp_frame` |  |
| `AutoLoginTests.answersNameThenPassword` | Ported | `core::login::the_name_then_the_password_are_answered_once_each` |  |
| `AutoLoginTests.answersAvatarMOTDWithBlankLine` | Ported | `core::login::a_banner_is_answered_with_a_blank_line` | On a made-up game: Avatar declares no login prompts; see gaps reported. |
| `AutoLoginTests.gamesWithoutMOTDFinishAfterPassword` | Ported | `core::login::a_games_own_prompts_match` |  |
| `AutoLoginTests.passwordlessProfileFinishesAfterName` | Ported | `core::login::a_character_with_no_saved_password_stops_after_the_name` | Bug 4. |
| `AutoLoginTests.customPromptsMatch` | Ported | `core::login::a_games_own_prompts_match` |  |

## TerminalTextTests.swift

16 tests: 4 ported, 11 already covered, 1 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `ANSIParserTests.colorThenReset` | Already covered | `unit::ansi::basic_and_bright_colors_and_bold` |  |
| `ANSIParserTests.boldAndBrightColors` | Already covered | `unit::ansi::basic_and_bright_colors_and_bold` |  |
| `ANSIParserTests.extendedColors` | Already covered | `unit::ansi::palette_colors_and_background`<br>`unit::ansi::truecolor_is_skipped_without_misreading_its_components` | Truecolor is dropped; see differences. |
| `ANSIParserTests.escapeSplitAcrossChunksIsReassembled` | Already covered | `unit::ansi::a_line_fed_in_two_pieces_parses_as_one` |  |
| `ANSIParserTests.styleCarriesIntoTheNextChunk` | Already covered | `unit::ansi::style_carries_to_the_next_line` |  |
| `ANSIParserTests.nonColorSequencesAreDropped` | Already covered | `unit::ansi::other_sequences_and_controls_are_removed` |  |
| `TerminalBufferTests.splitsLinesAndStripsCarriageReturns` | Already covered | `unit::line::every_line_ending_variant` |  |
| `TerminalBufferTests.unterminatedLineStaysVisible` | Already covered | `core-it::session::quiet_ends_a_prompt_on_a_server_with_no_ga` | The terminal holds finished lines; silence finishes this one. |
| `TerminalBufferTests.hidesPromptOnlyLinesAndPromptPrefixes` | Already covered | `core-it::defaults::prompt_lines_are_hidden_by_default`<br>`core-it::session::output_glued_after_a_prompt_shows_without_the_prompt` |  |
| `TerminalBufferTests.anUnlearnedPromptStaysVisible` | Ported | `core::terminal::a_prompt_that_was_never_learned_stays_visible` |  |
| `TerminalBufferTests.showsThePromptWhenAsked` | Not carried over | — | VYT-208: `Defaults::hide_prompts` is on until the settings sheet can turn it off. |
| `TerminalBufferTests.collapsesBlankLines` | Ported | `core::terminal::a_run_of_blank_lines_shows_as_one`<br>`core::terminal::blank_lines_around_a_hidden_prompt_show_as_one`<br>`core::terminal::a_blank_line_follows_a_command` | Bug 2. |
| `TerminalBufferTests.localEchoEndsTheInProgressLine` | Ported | `core::terminal::a_command_shows_after_the_line_in_progress`<br>`core::terminal::the_rest_of_a_line_shows_after_the_command_that_split_it`<br>`avatar::tables::a_command_mid_line_leaves_the_line_whole`<br>`avatar::tables::a_command_at_any_point_of_a_table_changes_nothing_read` | Bug 3. |
| `TerminalBufferTests.repliesAreNumberedAfterTheCommandTheyAnswer` | Ported | `core::terminal::a_reply_comes_after_the_command_it_answers`<br>`avatar::rooms::replies_follow_the_command_they_answer` | Bug 3. Ids became the store's order. |
| `TerminalBufferTests.colorsSurviveLineSplitting` | Already covered | `unit::ansi::style_carries_to_the_next_line` |  |
| `TerminalBufferTests.keepsAtMostMaxLines` | Already covered | `unit::scrollback::keeps_the_newest_lines_once_full` |  |

## TriggerTests.swift

7 tests: 0 ported, 0 already covered, 7 not carried over.

| Test | Outcome | Rust test | Note |
|---|---|---|---|
| `TriggerSetTests.plainTextMatchesAnywhereInAnyCase` | Not carried over | — | VYT-212: triggers. Plugin patterns and colors run, but the player's triggers don't exist yet. |
| `TriggerSetTests.regexMatches` | Not carried over | — | VYT-212: triggers. Plugin patterns and colors run, but the player's triggers don't exist yet. |
| `TriggerSetTests.badRegexAndOffTriggersNeverMatch` | Not carried over | — | VYT-212: triggers. Plugin patterns and colors run, but the player's triggers don't exist yet. |
| `TriggerSetTests.matchesCombine` | Not carried over | — | VYT-212: triggers. Plugin patterns and colors run, but the player's triggers don't exist yet. |
| `TerminalTriggerTests.hiddenLinesNeverReachTheChat` | Not carried over | — | VYT-212: triggers. Plugin patterns and colors run, but the player's triggers don't exist yet. |
| `TerminalTriggerTests.highlightsAFinishedLineAfterThePrompt` | Not carried over | — | VYT-212: triggers. Plugin patterns and colors run, but the player's triggers don't exist yet. |
| `TerminalTriggerTests.localLinesAreNotMatched` | Not carried over | — | VYT-212: triggers. Plugin patterns and colors run, but the player's triggers don't exist yet. |
