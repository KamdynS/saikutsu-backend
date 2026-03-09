use chrono::{Duration, NaiveDate};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    New,
    Learning,
    Review,
    Relearning,
}

impl FromStr for State {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "new" => Ok(State::New),
            "learning" => Ok(State::Learning),
            "review" => Ok(State::Review),
            "relearning" => Ok(State::Relearning),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rating {
    Again = 1,
    Hard = 2,
    Good = 3,
    Easy = 4,
}

#[derive(Debug, Clone)]
pub struct Card {
    pub state: State,
    pub difficulty: f64,
    pub stability: f64,
    pub due: Option<NaiveDate>,
    pub last_review: Option<NaiveDate>,
    pub reps: i32,
    pub lapses: i32,
}

#[derive(Debug, Clone)]
pub struct FSRSParams {
    pub w: [f64; 17],
    pub desired_retention: f64,
}

impl Default for FSRSParams {
    fn default() -> Self {
        Self {
            w: [
                0.4, 0.6, 2.4, 5.8, 4.93, 0.94, 0.86, 0.01, 1.49, 0.14, 0.94, 2.18, 0.05, 0.34,
                1.26, 0.29, 2.61,
            ],
            desired_retention: 0.9,
        }
    }
}

pub struct FSRS {
    params: FSRSParams,
}

impl FSRS {
    pub fn new(params: Option<FSRSParams>) -> Self {
        Self {
            params: params.unwrap_or_default(),
        }
    }

    pub fn review(&self, card: &Card, rating: Rating, today: NaiveDate) -> Card {
        match card.state {
            State::New => self.first_review(rating, today),
            State::Learning => self.learning_review(card, rating, today),
            State::Review => self.review_review(card, rating, today),
            State::Relearning => self.relearning_review(card, rating, today),
        }
    }

    fn first_review(&self, rating: Rating, today: NaiveDate) -> Card {
        let stability = self.params.w[rating as usize - 1];
        let difficulty = self.init_difficulty(rating);

        let (state, due) = if rating == Rating::Again || rating == Rating::Hard {
            (State::Learning, Some(today))
        } else {
            let interval = self.next_interval(stability);
            (
                State::Review,
                Some(today + Duration::days(interval as i64)),
            )
        };

        Card {
            state,
            difficulty,
            stability,
            due,
            last_review: Some(today),
            reps: 1,
            lapses: 0,
        }
    }

    fn review_review(&self, card: &Card, rating: Rating, today: NaiveDate) -> Card {
        let elapsed = card
            .last_review
            .map(|lr| (today - lr).num_days().max(0) as f64)
            .unwrap_or(0.0);

        let retrievability = self.retrievability(card.stability, elapsed as i32);

        if rating == Rating::Again {
            let stability = self.stability_after_lapse(card);
            let difficulty = self.next_difficulty(card.difficulty, rating);

            Card {
                state: State::Relearning,
                difficulty,
                stability,
                due: Some(today),
                last_review: Some(today),
                reps: card.reps + 1,
                lapses: card.lapses + 1,
            }
        } else {
            let stability = self.stability_after_success(card, rating, retrievability);
            let difficulty = self.next_difficulty(card.difficulty, rating);
            let interval = self.next_interval(stability);

            Card {
                state: State::Review,
                difficulty,
                stability,
                due: Some(today + Duration::days(interval as i64)),
                last_review: Some(today),
                reps: card.reps + 1,
                lapses: card.lapses,
            }
        }
    }

    fn learning_review(&self, card: &Card, rating: Rating, today: NaiveDate) -> Card {
        if rating == Rating::Again || rating == Rating::Hard {
            Card {
                state: State::Learning,
                due: Some(today),
                last_review: Some(today),
                reps: card.reps + 1,
                ..card.clone()
            }
        } else {
            let stability = self.stability_after_success(card, rating, 0.9);
            let interval = self.next_interval(stability);

            Card {
                state: State::Review,
                stability,
                due: Some(today + Duration::days(interval as i64)),
                last_review: Some(today),
                reps: card.reps + 1,
                ..card.clone()
            }
        }
    }

    fn relearning_review(&self, card: &Card, rating: Rating, today: NaiveDate) -> Card {
        if rating == Rating::Again {
            Card {
                state: State::Relearning,
                due: Some(today),
                last_review: Some(today),
                reps: card.reps + 1,
                ..card.clone()
            }
        } else {
            let stability = self.stability_after_success(card, rating, 0.9);
            let interval = self.next_interval(stability);

            Card {
                state: State::Review,
                stability,
                due: Some(today + Duration::days(interval as i64)),
                last_review: Some(today),
                reps: card.reps + 1,
                ..card.clone()
            }
        }
    }

    fn init_difficulty(&self, rating: Rating) -> f64 {
        let d = self.params.w[4] - (rating as i32 - 3) as f64 * self.params.w[5];
        d.clamp(1.0, 10.0)
    }

    fn next_difficulty(&self, difficulty: f64, rating: Rating) -> f64 {
        let delta = self.params.w[9] * (rating as i32 - 3) as f64;
        let new_d = difficulty + delta;
        let mean_reversion = self.params.w[10] * (self.params.w[4] - new_d);
        (new_d + mean_reversion).clamp(1.0, 10.0)
    }

    fn stability_after_success(&self, card: &Card, rating: Rating, retrievability: f64) -> f64 {
        let s = card.stability;
        let d = card.difficulty;

        let s_increase = self.params.w[6].exp()
            * (11.0 - d)
            * s.powf(-self.params.w[7])
            * ((1.0 - retrievability) * self.params.w[8]).exp_m1();

        let mut new_s = s * (1.0 + s_increase);

        new_s *= match rating {
            Rating::Hard => self.params.w[14],
            Rating::Easy => self.params.w[15],
            _ => 1.0,
        };

        new_s.max(0.1)
    }

    fn stability_after_lapse(&self, card: &Card) -> f64 {
        let s = card.stability;
        let d = card.difficulty;

        let new_s = self.params.w[11]
            * d.powf(-self.params.w[12])
            * ((s + 1.0).powf(self.params.w[13]) - 1.0);

        new_s.clamp(0.1, s)
    }

    fn retrievability(&self, stability: f64, elapsed_days: i32) -> f64 {
        if stability <= 0.0 {
            return 0.0;
        }
        0.9_f64.powf(elapsed_days as f64 / stability)
    }

    fn next_interval(&self, stability: f64) -> i32 {
        if stability <= 0.0 {
            return 1;
        }
        let interval = stability * self.params.desired_retention.ln() / 0.9_f64.ln();
        interval.round().max(1.0) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_review_good() {
        let fsrs = FSRS::new(None);
        let card = Card {
            state: State::New,
            difficulty: 0.0,
            stability: 0.0,
            due: None,
            last_review: None,
            reps: 0,
            lapses: 0,
        };

        let today = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let new_card = fsrs.review(&card, Rating::Good, today);

        assert_eq!(new_card.state, State::Review);
        assert_eq!(new_card.reps, 1);
        assert!(new_card.due.is_some());
    }

    #[test]
    fn test_first_review_again() {
        let fsrs = FSRS::new(None);
        let card = Card {
            state: State::New,
            difficulty: 0.0,
            stability: 0.0,
            due: None,
            last_review: None,
            reps: 0,
            lapses: 0,
        };

        let today = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let new_card = fsrs.review(&card, Rating::Again, today);

        assert_eq!(new_card.state, State::Learning);
        assert_eq!(new_card.reps, 1);
    }
}
