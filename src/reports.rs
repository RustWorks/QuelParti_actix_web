use chrono::NaiveDate;
use std::collections::HashMap;

use crate::config::structs::Platform;
use crate::database::structs::{Choice, ResultsGroupes, Submissions};
use crate::database::views::{
    ResultsParticipations, ResultsPublic, ResultsPublicGlobal, ResultsPublicGroupes,
};
use crate::errors::{throw, ErrorKind, InstanceError};
use crate::matching::{calculate_affinity, calculate_score, GroupMatch, UserOpinion};
use crate::DbConn;

// would've used tallystick but
// it's only available in Nightly
// also it doesn't support score voting with a median

// stats reports

//use crate::database::views;

pub fn generate_report(
    platform: &Platform,
    generated_at: NaiveDate,
    conn: &DbConn,
) -> Result<ResultsPublic, InstanceError> {
    // 1. check if the poll is still open
    if platform.end_at < generated_at {
        return Err(throw(
            ErrorKind::InfoPollClosed,
            format!("{}: {} - {}", platform.id, platform.end_at, generated_at),
        ));
    }

    // 2. get a count of the valid submissions
    let valid_sub_count =
        Submissions::count_valid(platform.id, conn, platform.begin_at, generated_at)
            .map_err(|e| throw(ErrorKind::CritReportCountValid, e.to_string()))?;

    // 3. check if the poll has reached its minimum participations
    if valid_sub_count < i64::from(platform.minimum_participations) {
        // if it didn't, do not generate a report
        return Err(throw(
            ErrorKind::InfoNotEnoughSubs,
            format!(
                "{}: {} / {}",
                platform.id, valid_sub_count, platform.minimum_participations
            ),
        ));
    }

    // 4. get the total submissions count
    let total_sub_count = Submissions::count(platform.id, conn)
        .map_err(|e| throw(ErrorKind::CritReportCountTotal, e.to_string()))?;

    // 5. get all the valid submissionschoices!
    let subchoices_list =
        Submissions::get_valid(platform.id, conn, platform.begin_at, generated_at)
            .map_err(|e| throw(ErrorKind::CritReportGetValid, e.to_string()))?;

    // 6. basically, do a GROUP BY
    let mut submissions_list: HashMap<i64, Vec<UserOpinion>> = HashMap::new();

    let mut prev_subid = -1;
    for subc in subchoices_list {
        let new_vote = match Choice::from_db(subc.userchoice) {
            Some(v) => v,
            None => {
                // should never happen
                return Err(throw(
                    ErrorKind::FatalUnmatchedChoice,
                    subc.userchoice.to_string(),
                ));
            }
        };
        // ... if we're still on the same submission
        if prev_subid == subc.submission_id {
            // add the submission to list, unless it doesn't find it
            match submissions_list.get_mut(&prev_subid) {
                Some(v) => v.push(UserOpinion {
                    question_id: subc.question_id,
                    vote: new_vote,
                }),
                None => {
                    return Err(throw(
                        ErrorKind::FatalMissingSubmission,
                        format!("{:?}", subc),
                    ));
                }
            }
        } else {
            prev_subid = subc.submission_id;
            let new_sublist: Vec<UserOpinion> = vec![UserOpinion {
                question_id: subc.question_id,
                vote: new_vote,
            }];
            submissions_list.insert(subc.submission_id, new_sublist);
        }
    }

    // 7. calculate all the scores
    let mut all_scores: HashMap<i64, Vec<GroupMatch>> = HashMap::new();
    for (subid, scores) in submissions_list {
        let entry_score = match calculate_score(platform, &scores) {
            Some(v) => v,
            None => {
                return Err(throw(
                    ErrorKind::FatalInvalidScoreCalc,
                    format!("{:?}", scores),
                ));
            }
        };

        // convert score to affinity
        let entry_score = match calculate_affinity(&entry_score, scores.len()) {
            Some(v) => v,
            None => {
                return Err(throw(
                    ErrorKind::FatalInvalidScoreConv,
                    format!("{:?}", scores),
                ));
            },
        };
        all_scores.insert(subid, entry_score);
    }

    // 8. calculate the median
    let mut results_groups: Vec<ResultsPublicGroupes> = Vec::new();

    for group in &platform.groups {
        results_groups.push(ResultsPublicGroupes {
            id: *group,
            value_median: calc_median(*group, &all_scores),
        });
    }

    // 9. pack it together
    Ok(ResultsPublic {
        global: ResultsPublicGlobal {
            platform_id: platform.id,
            started_at: platform.begin_at,
            generated_at,
            participations: ResultsParticipations {
                total: total_sub_count,
                valid: valid_sub_count,
            },
        },
        groupes: results_groups,
    })
}

// Score calculation method: Majority judgement
fn calc_median(group_id: i16, all_scores: &HashMap<i64, Vec<GroupMatch>>) -> f32 {
    let mut group_allscores: Vec<f32> = Vec::new();
    for gv in all_scores.values() {
        // explore the Vec<GroupScore> element
        match gv.iter().find(|g| g.id == group_id) {
            Some(v) => group_allscores.push(v.affinity),
            None => {
                // don't quit if the group isn't found
                eprintln!(
                    "warn: couldn't find one of the groups during calc: {:?}",
                    group_allscores
                );
            }
        }
    }
    // sort'em all
    group_allscores.sort_by(|a, b| a.partial_cmp(b).expect("There's a NaN!!!"));

    // return the median
    if let Some(v) = group_allscores.get(group_allscores.len() / 2) { *v } else {
        eprintln!(
            "error: couldn't calculate the median score for a group: {:?}",
            group_allscores
        );
        0.0
    }
}

// hide sensitive data that may help to guess
// which tries are valid
pub fn round_data(number: i64) -> i64 {
    match number {
        _ if number % 10 == 0 => number,
        _ => number - number % 10,
    }
}

pub fn groups_to_public(groups: &[ResultsGroupes]) -> Vec<ResultsPublicGroupes> {
    let mut new_groups: Vec<ResultsPublicGroupes> = Vec::new();

    for group in groups {
        new_groups.push(ResultsPublicGroupes {
            id: group.group_id,
            value_median: group.value_median,
        });
    }

    new_groups
}
