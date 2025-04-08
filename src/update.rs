#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals)]

extern crate ical;

use shuttle_runtime::SecretStore;

use std::sync::Arc;

use futures::future::join_all;

use chrono_tz::Tz;

use ical::parser::ical::component::{IcalCalendar, IcalEvent};

use serde_json::{json, Value};

use chrono::{NaiveDate, NaiveDateTime, DateTime, Duration, Utc, Timelike, Days};
use chrono_tz::Europe::Oslo;

use reqwest::Method;

use tokio::task;

use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};

pub async fn send_email(
    secretStore: &SecretStore,
    subject: &str, 
    body: &str
) -> Result<(), Box<dyn std::error::Error>> {
    let smtp_user = secretStore.get("SMTP_USER").expect("Sett SMTP_USER");
    let smtp_pass = secretStore.get("SMTP_PASS").expect("Sett SMTP_USER");
    let smtp_server = secretStore.get("SMTP_SERVER").expect("Sett SMTP_USER");

    let email = Message::builder()
        .from(smtp_user.parse()?)
        .to(smtp_user.parse()?) // Send to yourself
        .subject(subject)
        .body(body.to_string())?;

    let creds = Credentials::new(smtp_user, smtp_pass);
    let mailer = SmtpTransport::relay(&smtp_server)?.credentials(creds).build();

    mailer.send(&email)?;
    Ok(())
}

static deleteEmoji: char = '❌';

#[derive(Debug)]
#[derive(PartialEq)]
#[derive(Clone)]
enum StartEnd {
    Date(NaiveDate),
    DateDate(NaiveDate, NaiveDate),
    DateTime(DateTime<Tz>, DateTime<Tz>)
}

impl StartEnd {
    /// Returne det som burde stå på "date": ... i notion requests
    fn getNotionDate(&self) -> Value {
        match self {
            StartEnd::Date(dato) => {
                json!({
                    "start": dato.to_string(),
                })
            },
            StartEnd::DateDate(start, slutt) => {
                json!({
                    "start": start.to_string(),
                    "end": slutt.to_string(),
                })
            },
            StartEnd::DateTime(start, slutt) => {
                json!({
                    "start": start.to_rfc3339()[..19],
                    "end": slutt.to_rfc3339()[..19],
                    "time_zone": "Europe/Oslo"
                })
            }
        }
    }
}

#[derive(Debug)]
#[derive(Clone)]
struct EventProps {
    startEnd: StartEnd,
    title: String,
    varighet: Option<i32>,
    beskrivelse: Option<String>,
    sted: Option<String>,
    uid: Option<String>,
    pageId: Option<String>,
    done: bool // I iCal e dette alltid false, i Notion er dette om eventen er krysset av for
}

impl PartialEq for EventProps {
    fn eq(&self, other: &Self) -> bool {
        self.startEnd == other.startEnd &&
        self.title == other.title &&
        self.varighet == other.varighet &&
        self.beskrivelse == other.beskrivelse &&
        self.sted == other.sted &&
        self.uid == other.uid
    }
}

impl EventProps  {
    fn getProperty(event: &IcalEvent, propertyName: &str) -> Option<String> {
        event.properties.iter().find(|p| p.name == propertyName)?
            .clone()
            .value
    }

    fn getDateTimeVarighet (startEnd: &StartEnd) -> Option<i32> {
        match startEnd {
            StartEnd::Date(_) => None,
            StartEnd::DateDate(_, _) => None,
            StartEnd::DateTime(start, end) => {
                let varighet = end.clone().signed_duration_since(start.clone()).num_minutes();

                if varighet <= 720 { Some(varighet as i32) } else { None }
            },
        }
    }

    fn notionJsonName(title: &str) -> Value {
        json!({
            "title": [
                {
                    "text": {
                        "content": title
                    }
                }
            ]
        })
    }

    fn notionJson(&self, livsdelID: &str) -> Value {
        json!({
            "Name": EventProps::notionJsonName(&self.title),
            "Dato": {
                "date": self.startEnd.getNotionDate()
            },
            "Minutt": {
                "number": self.varighet
            },
            "Livsdel": {
				"relation": [
					{
						"id": livsdelID
					}
				]
			}
        })
    }

    fn getStartEndiCal(event: &IcalEvent) -> StartEnd {
        let mut start = EventProps::getProperty(event, "DTSTART").expect("Should have start");
        let end = EventProps::getProperty(event, "DTEND");

        if start.len() == 8 {
            if end.is_some() && start != end.clone().unwrap() {
                return StartEnd::DateDate(
                    NaiveDate::parse_from_str(&start, "%Y%m%d").expect(format!("Couldn't parse date start {start}").as_str()), 
                    NaiveDate::parse_from_str(&end.clone().unwrap(), "%Y%m%d").expect(format!("Couldn't parse date end {}", end.clone().unwrap()).as_str()).checked_sub_days(Days::new(1)).unwrap()
            );
            } else {
                return StartEnd::Date(
                    NaiveDate::parse_from_str(&start, "%Y%m%d").expect(format!("Couldn't parse date startend {start}").as_str()));
            }
        }

        // Dette håndtere bare UTC, eller Oslo timezone, som holder for no
        // MyTXS generere bare Oslo timezone pr no, og vil i framtida potensielt generer UTC
        let utc: bool = start.ends_with("Z");
        if utc { start.pop(); }

        // Str to NaiveDateTime
        let start = NaiveDateTime::parse_from_str(&start, "%Y%m%dT%H%M%S")
            .expect(format!("Couldn't parse datetime start {start}").as_str());

        let end = if end.is_none() {
            start + Duration::hours(1)
        } else {
            let mut end = end.unwrap();
            if utc { end.pop(); }
            NaiveDateTime::parse_from_str(&end, "%Y%m%dT%H%M%S")
                .expect(format!("Couldn't parse datetime end {start}").as_str())
        };

        // Gjør at end før 4 på natta endres endres til 23:59 forrige dag
        let end = if end.hour() < 4 && start.date() + Duration::days(1) == end.date() {
            start.date().and_hms_opt(23, 59, 0).unwrap()
        } else {
            end
        };

        // NaiveDateTime to DateTime
        let start = if utc {
            start.and_local_timezone(Utc).unwrap().with_timezone(&Oslo)
        } else { start.and_local_timezone(Oslo).unwrap() };
        let end = if utc {
            end.and_local_timezone(Utc).unwrap().with_timezone(&Oslo)
        } else { end.and_local_timezone(Oslo).unwrap() };

        StartEnd::DateTime(start, end)
    }

    async fn makeEventsFromIcal(client: &reqwest::Client, icalLink: &str, livsdelEmoji: &char) -> Vec<EventProps> {
        let resp = client.get(icalLink).send()
            .await.expect("icalResponse").text().await.expect("icalContent");

        let cal: IcalCalendar = ical::IcalParser::new(resp.as_bytes()).next()
            .expect("Det er en kalender her").expect("Det er en kalender her 2");
        
        let mut events: Vec<EventProps> = Vec::new();

        for icalEvent in cal.events {
            let startEnd = EventProps::getStartEndiCal(&icalEvent);

            match startEnd {
                StartEnd::Date(startDate) => {
                    if startDate < Utc::now().naive_utc().date() - Duration::days(1) {
                        continue
                    }
                },
                StartEnd::DateDate(startDate, _) => {
                    if startDate < Utc::now().naive_utc().date() - Duration::days(1) {
                        continue
                    }
                },
                StartEnd::DateTime(startDateTime, _) => {
                    if startDateTime.naive_utc() < Utc::now().naive_utc() - Duration::days(1) {
                        continue
                    }
                }
            }
            
            events.push(EventProps {
                varighet: EventProps::getDateTimeVarighet(&startEnd),
                startEnd: startEnd,
                title: format!("{}{}", livsdelEmoji.to_string(), EventProps::getProperty(&icalEvent, "SUMMARY").expect("Should have title")),
                beskrivelse: EventProps::getProperty(&icalEvent, "DESCRIPTION")
                    .filter(|b| !b.trim().is_empty())
                    .map(|b| str::replace(&b, "\\n", "\n")),
                sted: EventProps::getProperty(&icalEvent, "LOCATION").filter(|p| !p.trim().is_empty()),
                uid: Some(EventProps::getProperty(&icalEvent, "UID").expect("All events have UID")),
                pageId: None,
                done: false
            });
        }

        events
    }

    fn getStartEndNotion(page: &Value) -> StartEnd {
        let date = page["properties"]["Dato"]["date"].clone();
        let start = date["start"].as_str().expect("Should have start str");
        let end = date["end"].as_str();

        if start.len() == 10 {
            if end.is_some() && end.unwrap() != start {
                return StartEnd::DateDate(
                    NaiveDate::parse_from_str(&start, "%Y-%m-%d").expect("Should have start"),
                    NaiveDate::parse_from_str(&end.unwrap(), "%Y-%m-%d").expect("Should have end")
                );
            } else {
                return StartEnd::Date(NaiveDate::parse_from_str(&start, "%Y-%m-%d").expect("Should have start 2"));
            }
        }

        if end == None {
            StartEnd::DateTime(
                DateTime::parse_from_rfc3339(&start)
                    .expect(format!("Couldn't parse date 1 {start}").as_str())
                    .with_timezone(&Oslo),
                DateTime::parse_from_rfc3339(&start)
                    .expect(format!("Couldn't parse date 2 {start}").as_str())
                    .with_timezone(&Oslo) + Duration::hours(1)
            );
        }

        StartEnd::DateTime(
            DateTime::parse_from_rfc3339(&start)
                .expect(format!("Couldn't parse date 3 {start}").as_str())
                .with_timezone(&Oslo),
            DateTime::parse_from_rfc3339(end.unwrap())
                .expect(format!("Couldn't parse date 4 {start}").as_str())
                .with_timezone(&Oslo)
        )
    }

    fn getLastCommentWithStart(comments: &Vec<Value>, start: &str) -> Option<String>  {
        let comment = comments.iter().rev()
            .find(|c| c["rich_text"][0]["plain_text"].as_str().expect("Comment").starts_with(start))
            .map(|c| String::from(c["rich_text"][0]["plain_text"].as_str().expect("CommentNotFound").strip_prefix(start).unwrap()));

        if comment.is_none() || comment.clone().unwrap().is_empty() { return None }

        comment
    }
    
    async fn makeEventsFromNotion(notionClient: &NotionClient) -> Vec<EventProps> {
        let pages = notionClient.getBotPages().await;

        // Create a vector of futures
        let futures: Vec<_> = pages.iter()
            .map(|page| notionClient.getComments(page["id"].as_str()
                .expect("All pages have an ID")))
            .collect();

        // Run all futures in parallel and wait for all to complete
        let comments_results: Vec<Vec<Value>> = join_all(futures).await;

        let mut events: Vec<EventProps> = Vec::new();

        for (page, comments) in pages.iter().zip(comments_results.iter()) {
            let startEnd = EventProps::getStartEndNotion(&page);

            let title = String::from(page["properties"]["Name"]["title"][0]["plain_text"].as_str().expect("Page must have title"));

            if title.contains(deleteEmoji) { continue }

            events.push(EventProps {
                varighet: page["properties"]["Minutt"]["number"].as_i64().map(|d| d as i32),
                startEnd: startEnd,
                title:  title,
                beskrivelse: EventProps::getLastCommentWithStart(comments, "Beskrivelse: "),
                sted: EventProps::getLastCommentWithStart(comments, "Sted: "),
                uid: EventProps::getLastCommentWithStart(comments, "UID: "),
                pageId: page["id"].as_str().map(|s| String::from(s)),
                done: page["properties"][""]["checkbox"].as_bool().expect("Should have checkbox")
            });
        }

        events
    }
}


struct NotionClient {
    apiToken: String,
    integrationUserID: String,
    trackingDBID: String,
    reqwestClient: reqwest::Client,
}


impl NotionClient {
    fn new (apiToken: String, integrationUserID: String, trackingDBID: String) -> NotionClient {
        NotionClient {apiToken, integrationUserID, trackingDBID, reqwestClient: reqwest::Client::new()}
    }

    fn createdByFilter (&self) -> Value {
        json!({
            "property": "created_by",
            "people": {
                "contains": self.integrationUserID
            }
        })
    }

    // fn notDoneFilter (&self) -> Value {
    //     json!({
    //         "property": "",
    //         "checkbox": {
    //             "does_not_equal": true
    //         }
    //     })
    // }

    fn notOldFilter (&self) -> Value {
        json!({
            "property": "Dato",
            "date": {
                "on_or_after": Utc::now().naive_utc().date() - Duration::days(1)
            }
        })
    }

    // fn createdByAndNotDoneFilter (&self) -> serde_json::Value {
    //     json!({ "and": [self.notDoneFilter(), self.createdByFilter()] })
    // }

    fn createdByAndNotOldFilter (&self) -> serde_json::Value {
        json!({ "and": [self.notOldFilter(), self.createdByFilter()] })
    }

    async fn makeRequest (&self, method: Method, path: &str, body: Value) -> Value {
        let resJson: Value = self.reqwestClient.request(method.clone(), format!("https://api.notion.com/v1/{path}"))
            .header("Authorization", format!("Bearer {}", self.apiToken))
            .header("Notion-Version", "2022-06-28")
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send().await
            .expect("Response plz")
            .json().await
            .expect("JSON plz");

        // Dersom noko e feil vil notion respond med code=validation_error, så print isåfall
        if resJson["code"].as_str() == Some("validation_error") {
            println!("Error with {method} on {path} with {body}");
            println!("{:#?}", resJson)
        }

        resJson
    }

    async fn getBotPages (&self) -> Vec<Value> {
        let mut result: Vec<Value> = Vec::new();

        let mut response = self.makeRequest(
            Method::POST, 
            &format!("databases/{}/query", self.trackingDBID), 
            json!({
                "filter": self.createdByAndNotOldFilter(),
            })
        ).await;

        loop {
            result.extend(response["results"].as_array().expect("Results should be a list").to_owned());

            if response["has_more"].as_bool().expect("HasMoreBool") {
                response = self.makeRequest(
                    Method::POST, 
                    &format!("databases/{}/query", self.trackingDBID), 
                    json!({
                        "filter": self.createdByAndNotOldFilter(),
                        "start_cursor": response["next_cursor"].as_str().expect("Should have next_cursor")
                    })
                ).await;
            } else { break }
        }
        result
    }

    async fn getComments (&self, pageId: &str) -> Vec<Value> {
        self.makeRequest(
            Method::GET, 
            format!("comments?block_id={pageId}").as_str(), 
            json!({})
        )
            .await["results"]
            .as_array().expect("Array plz").to_owned()
    }

    async fn hasChildren (&self, pageId: &str) -> bool {
        !self.makeRequest(
            Method::GET, 
            format!("blocks/{pageId}/children").as_str(), 
            json!({})
        )
            .await["results"]
            .as_array().expect("Array plz 1").is_empty()
    }

    async fn hasUserComments (&self, pageId: &str) -> bool {
        self.makeRequest(
            Method::GET, 
            format!("comments?block_id={pageId}").as_str(), 
            json!({})
        )
            .await["results"]
            .as_array().expect("Array plz 2").to_owned().iter().any(
                |c| c["created_by"]["id"].as_str().expect("CommentHasAuthor") != self.integrationUserID
            )
    }

    /// Slett en page dersom den ikkje har innhold eller (kommentara fra nån andre enn botten)
    async fn softDelete (&self, notionEvent: &EventProps) {
        if self.hasChildren(notionEvent.pageId.as_ref().unwrap()).await || 
        self.hasUserComments(notionEvent.pageId.as_ref().unwrap()).await {
            // Marker som slettet ved å hiv deleteEmoji i title
            let mut title = notionEvent.title.clone();
            title.insert(notionEvent.title.char_indices().nth(1).expect("Title should have more than 1 characters").0, deleteEmoji);
            self.makeRequest(
                Method::PATCH, 
                format!("pages/{}", notionEvent.pageId.as_ref().unwrap()).as_str(), 
                json!({
                    "properties": {
                        "Name": EventProps::notionJsonName(&title)
                    }
                })
            ).await;

            println!("Soft deleting {}", notionEvent.title)
        }else{
            // Faktisk slett siden
            self.makeRequest(
                Method::PATCH, 
                format!("pages/{}", notionEvent.pageId.as_ref().unwrap()).as_str(), 
                json!({
                    "archived": true
                })
            ).await;

            println!("Deleted {}", notionEvent.title);
        }
    }

    /// Lag en kommentar
    async fn comment(&self, pageId: &str, prefix: &str, comment: &str) {
        self.makeRequest(
            Method::POST, 
            "comments", 
            json!({
                "parent": {
                  "page_id": pageId
                },
                "rich_text": [
                  {
                    "text": {
                      "content": format!("{prefix}{comment}")
                    }
                  }
                ]
            })
        ).await;
    }

    async fn createPage (&self, event: &EventProps, livsdelID: &str) {
        let pageId = String::from(self.makeRequest(
            Method::POST, 
            format!("pages").as_str(), 
            json!({
                "parent": {
                    "database_id": self.trackingDBID
                },
                "properties": event.notionJson(livsdelID)
            })
        ).await["id"].as_str().expect("PageID"));

        self.comment(
            &pageId,
            "UID: ", 
            event.uid.as_ref().unwrap().as_str()
        ).await;

        if event.sted.is_some() {
            self.comment(
                &pageId,
                "Sted: ", 
                event.sted.as_ref().unwrap().as_str()
            ).await
        };

        if event.beskrivelse.is_some() {
            self.comment(
                &pageId,
                "Beskrivelse: ", 
                event.beskrivelse.as_ref().unwrap().as_str()
            ).await
        };
    }

    async fn updatePage (&self, icalEvent: &EventProps, notionEvent: &EventProps, livsdelID: &str) {
        self.makeRequest(
            Method::PATCH, 
            format!("pages/{}", notionEvent.pageId.as_ref().unwrap()).as_str(), 
            json!({
                "properties": icalEvent.notionJson(livsdelID)
            })
        ).await;

        if icalEvent.sted != notionEvent.sted {
            self.comment(
                &notionEvent.pageId.as_ref().unwrap().as_str(),
                "Sted: ", 
                icalEvent.sted.as_ref().unwrap_or(&String::from("")).as_str()
            ).await
        };

        if icalEvent.beskrivelse != notionEvent.beskrivelse {
            self.comment(
                &notionEvent.pageId.as_ref().unwrap().as_str(),
                "Beskrivelse: ", 
                icalEvent.beskrivelse.as_ref().unwrap_or(&String::from("")).as_str()
            ).await
        };
    }
}

pub async fn update(
    secretStore: &SecretStore
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Fiks environment variables
    let client = reqwest::Client::new();

    let notionClient = Arc::new(NotionClient::new(
        secretStore.get("NOTION_API_TOKEN").expect("Sett NOTION_API_TOKEN"), 
        secretStore.get("INTEGRATION_USER_ID").expect("Sett INTEGRATION_USER_ID"), 
        secretStore.get("TRACKING_DATABASE").expect("Sett TRACKING_DATABASE"), 
    ));

    let icalLinks: Vec<String> = secretStore.get("ICAL_LINKS").expect("Sett ICAL_LINKS").split(",").map(|s| String::from(s)).collect();
    let livsdelIDs: Vec<String> = secretStore.get("LIVSDEL_IDS").expect("Sett LIVSDEL_IDS").split(",").map(|s| String::from(s)).collect();

    let mut notionEvents = EventProps::makeEventsFromNotion(&notionClient).await;

    let mut matchingUIDs: Vec<String> = Vec::new();

    for (icalLink, livsdelID) in icalLinks.iter().zip(livsdelIDs.iter()) {
        let livsdelEmoji: char = notionClient.makeRequest(Method::GET, &format!("pages/{livsdelID}"), json!({})).await
            ["icon"]["emoji"].as_str().expect("Livsdel must have emoji").chars().next().expect("Emoji must be at least one char");

        let mut icalEvents = EventProps::makeEventsFromIcal(&client, &icalLink, &livsdelEmoji).await;

        let mut tasks: Vec<_> = Vec::new();
        for icalEvent in icalEvents.clone() {
            let notionEvent = notionEvents.iter()
                .find(|notionEvent| icalEvent.uid == notionEvent.uid);

            if notionEvent.is_none() { continue }

            // Her har vi matchende events
            let notionEvent = notionEvent.expect("notionEvent?").clone();

            matchingUIDs.push(icalEvent.uid.clone().expect("icalEventUid"));

            if notionEvent.done || icalEvent == notionEvent { continue }

            // Async fuckery
            let livsdelID = livsdelID.clone();
            let notionClient = Arc::clone(&notionClient);
            tasks.push(task::spawn(async move {
                notionClient.updatePage(&icalEvent, &notionEvent, &livsdelID).await;
                println!("Updated {} {:?}", icalEvent.title, icalEvent.startEnd)
            }))
        }

        join_all(tasks).await;

        let mut tasks: Vec<_> = Vec::new();
        icalEvents = icalEvents.into_iter().filter(|e| !matchingUIDs.contains(e.uid.as_ref().expect("iCalUID?"))).collect();
        for icalEvent in icalEvents {
            // Her har vi bare icalEvents

            // Async fuckery
            let notionClient = Arc::clone(&notionClient);
            let livsdelID = livsdelID.clone();
            tasks.push(task::spawn(async move {
                notionClient.createPage(&icalEvent, &livsdelID).await;
                println!("Created {} {:?}", icalEvent.title, icalEvent.startEnd);
            }))
        }
        join_all(tasks).await;
    }
    
    let mut tasks: Vec<_> = Vec::new();

    notionEvents = notionEvents.into_iter().filter(|e: &EventProps| e.uid.is_none() || !matchingUIDs.contains(e.uid.as_ref().expect("notionUID?"))).collect();
    for notionEvent in notionEvents {
        // Her har vi bare notionEvents

        if notionEvent.done { continue }

        // Async fuckery
        let notionClient = Arc::clone(&notionClient);
        tasks.push(task::spawn(async move {
            notionClient.softDelete(&notionEvent).await;
        }))
    }
    join_all(tasks).await;
    
    Ok(())
}
