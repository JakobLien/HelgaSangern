#![allow(non_snake_case)]

use std::sync::Arc;

use maud::{html, DOCTYPE};
use rocket::{fs::{relative, FileServer}, get, response::{content::RawHtml, Redirect}, routes, uri, State};

use shuttle_rocket::ShuttleRocket;
use shuttle_runtime::SecretStore;

use reqwest::{cookie::{self, CookieStore}, Client, Url};

use serde_json::{json, Value};

use chrono::{DateTime, Datelike, Days, Duration as ChronoDuration, Locale, NaiveDate, NaiveDateTime, Timelike, Utc};

use scraper::{Html, Selector};

use tokio::time::{interval, Duration as TokioDuration};

#[derive(sqlx::FromRow)]
struct KeyValue {
    #[allow(dead_code)] // Denne e ikkje dead code, Rust bare ser ikkje ka vi bruke det te. 
    key: String,
    value: String
}

trait GetSet {
    async fn get(&self, key: &str) -> String;
    async fn set(&self, key: &str, value: &str);
}

// Vi bruke bare postgres som en key_value store her, med følgende nyttige metoda:)
// For å få sqlx te å funk e det fint å hiv inn følgende i en .env fil
// DATABASE_URL=postgres://postgres:postgrespassword@localhost:5432/helgasangerntest
impl GetSet for sqlx::PgPool {
    /// Hjelpemetode som lar oss behandle postgres som en key_value store
    async fn get(&self, key: &str) -> String {
        let res = sqlx::query_as!(KeyValue, "SELECT * FROM key_value WHERE key = $1", key)
            .fetch_optional(self)
            .await
            .unwrap();
    
        res.map(|r| r.value).unwrap_or("".to_string())
    }

    /// Hjelpemetode som lar oss behandle postgres som en key_value store
    async fn set(&self, key: &str, value: &str) {
        sqlx::query_as!(KeyValue, "INSERT INTO key_value (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = $2", key, value)
            .execute(self)
            .await
            .unwrap();
    }
}


const ROM_PRIORITERING: [&str; 12] = [
    // "04-065", // Originale HelgaSangern, 40 plassa, undervisningsrom
    // "03-023", // 50 plassa, undervisningsrom
    // "03-058", // 50 plassa, undervisningsrom
    // "03-033", // 30 plassa, grupperom
    // "03-047", // 30 plassa, grupperom
    // "04-072", // 30 plassa, grupperom
    // "04-067", // 30 plassa, undervisningsrom
    // "03-045", // 26 plassa, undervisningsrom
    // "04-023", // 25 plassa, undervisningsrom
    // "04-086", // 18 plassa, grupperom
    // "05-118", // 16 plassa, grupperom
    // "05-119", // 16 plassa, grupperom

    // For testing, book dem minste romman vi finn
    "05-063", 
    "04-088", 
    "04-089",
    "04-091",
    "04-092",
    "04-082",
    "04-098",
    "04-099",
    "03-078",
    "05-115",
    "05-116",
    "03-081",
];

const BOOKING_NAVN: &str = "HelgaSangern Kollokvie";
const BOOKING_UKEDAGER: [u8; 5] = [0, 1, 2, 3, 4];

/// Gitt en datetime generere denne neste datetime vi ønske å book på:)
fn getNextBooking(
    dateTime: &NaiveDateTime
) -> NaiveDateTime {
    if dateTime.hour() < 12 {
        dateTime.with_hour(13).unwrap().with_minute(0).unwrap()
    } else {
        let mut newDateTime = dateTime.with_hour(8).unwrap().with_minute(30).unwrap();
        newDateTime = newDateTime.checked_add_days(Days::new(1)).unwrap();
        while !BOOKING_UKEDAGER.contains(&(newDateTime.weekday().num_days_from_monday() as u8)){
            newDateTime = newDateTime.checked_add_days(Days::new(1)).unwrap();
        }
        newDateTime
    }
}

// API hjelpemetoder
trait ClientMethods {
    // async fn login(&self, secretStore: &SecretStore, pool: &sqlx::PgPool);
    async fn getBookings(&self) -> Result<Value, reqwest::Error>;
    async fn getScheduleForRoom(&self, room: &str, startDate: NaiveDate, endDate: NaiveDate) -> Result<Value, reqwest::Error>;
    async fn bookRoom(&self, roomName: &str, dateTime: &NaiveDateTime) -> Result<reqwest::Response, reqwest::Error>;
}

impl ClientMethods for Client {
    /// Skaffe våre egne bookinga
    async fn getBookings(&self) -> Result<Value, reqwest::Error> {
        self.get("https://tp.educloud.no/ntnu/ws/rombestilling/bookings.php")
                .header("accept", "application/json")
                .send().await.unwrap().json().await
    }

    /// Skaffe timeplan for et spesifikt rom
    async fn getScheduleForRoom(&self, room: &str, startDate: NaiveDate, endDate: NaiveDate) -> Result<Value, reqwest::Error> {
        self.get(format!("https://tp.educloud.no/ntnu/ws/1.4/room.php?id=250{}&fromdate={}&todate={}&lang=no&split_intervals=false",
                room, startDate.format("%F"), endDate.format("%F")
            ))
            .header("accept", "application/json")
            .send().await.unwrap().json().await
    }

    /// Booke faktisk rom på det tidspunktet
    async fn bookRoom(&self, roomName: &str, dateTime: &NaiveDateTime) -> Result<reqwest::Response, reqwest::Error>{
        self.post("https://tp.educloud.no/ntnu/ws/rombestilling/reservation.php")
            .header("accept", "application/json")
            .body(json!({
                "start": format!("{}", dateTime.format("%FT%T")),
                "end": format!("{}", dateTime.checked_add_signed(ChronoDuration::hours(4)).unwrap().format("%FT%T")),
                "rooms": [format!("250{}", roomName)], // Romnavn formateres som campus/byggnavn (250 for Helgasetr), etterfulgt av navnet
                "name": BOOKING_NAVN,
                "notes": "",
                "userGroup": null
            }).to_string())
            .send().await
    }
}

/// En metode som kjøre heile rombookingsprosessen, heilt fra vi har en innlogget client, 
/// til å finn ut hvilke rom vi skal booke, til å faktisk booke dem. 
async fn bookRooms(secretStore: &SecretStore, pool: &sqlx::PgPool) {
    let (client, bookings) = getClientAndBookings(&secretStore, &pool).await;

    let today = Utc::now().date_naive();

    let mut bookingTimes: Vec<NaiveDateTime> = Vec::new();

    // Se på egne bookings
    for booking in bookings {
        if booking.get("name").unwrap().as_str().unwrap() == BOOKING_NAVN {
            let currDate = NaiveDateTime::parse_from_str(booking.get("booked").unwrap().as_str().unwrap(), "%F %T").unwrap();
            bookingTimes.push(currDate);
        }
    }
    bookingTimes.sort_by(|a, b| a.partial_cmp(b).unwrap());

    println!("bookingTimes: {:#?}", bookingTimes);

    // Generer liste av ting vi ønske å booke
    let mut newBookingTimes: Vec<NaiveDateTime> = Vec::new();
    newBookingTimes.push(bookingTimes.last().unwrap_or(&Utc::now().naive_local()).clone());
    for _i in 0..(8-bookingTimes.len()) {
        newBookingTimes.push(getNextBooking(newBookingTimes.last().unwrap()))
    }
    newBookingTimes.remove(0);

    println!("newBookingTimes: {:#?}", newBookingTimes);

    // Genrer liste av når romman e ledig
    let mut romSchedules:Vec<Value>  = Vec::new();

    for newBookingTime in newBookingTimes {
        for (roomIndex, roomName) in ROM_PRIORITERING.iter().enumerate() {
            if roomIndex >= romSchedules.len() {
                // Her har vi ikkje sendt request for dette rommet ennå
                romSchedules.push(client.getScheduleForRoom(roomName, today, today.checked_add_days(Days::new(14)).unwrap())
                .await.unwrap().get("events").unwrap().clone())
            }

            let roomSchedule = romSchedules.get(roomIndex).unwrap().clone();

            // println!("roomSchedule: {:#?}", roomSchedule);

            // Her vil hvert element i roomAvailability vær en liste av events, og romAvailability har alle rom te og med nåværende rommet. 

            let mut isFree = true;

            // println!("roomSchedule: {}", roomSchedule.to_string());

            // Sjekk om nån events i rommet skjer idag. Isåfall, continue. 
            for event in roomSchedule.as_array().unwrap() {
                // if 
                let start = DateTime::parse_from_str(event.get("dtstart").unwrap().as_str().unwrap(), "%FT%T%#z").unwrap();
                let end = DateTime::parse_from_str(event.get("dtend").unwrap().as_str().unwrap(), "%FT%T%#z").unwrap();
                let bookingName = event.get("summary").unwrap().as_str().unwrap();
                // println!("Conditionals: {} {} {} ", bookingName != bookingNavn, start.date_naive() <= newBookingTime.date(), newBookingTime.date() <= end.date_naive());
                // println!("Booking: {} {} {}", bookingName, start.date_naive(), end.date_naive());

                if bookingName != BOOKING_NAVN && start.date_naive() <= newBookingTime.date() && newBookingTime.date() <= end.date_naive() {
                    isFree = false;
                    break;
                }
            }

            if isFree {
                println!("Free: {}", roomName);
                // Om ingen events i det rommet skjer på denne dagen, book det
                println!("Booking response: {:#?}", client.bookRoom(roomName, &newBookingTime).await.unwrap().text().await.unwrap());
                break;
            } else {
                println!("Not free: {}", roomName);
            }
        }
    }
}


const KEY_TP_COOKIE: &str = "TP_COOKIE_KEY";

/// Returne og sett cookies for en innlogget reqwest::Client. 
/// Denne tråkke gjennom dem samme requestsa kæm som helst andre gjør når dem logge inn med feide. 
/// Det funke, men bli fort ødelagt det øyeblikket noko som helst med feide innlogginga endre seg. 
/// Samtidig e det trolig ikkje så ofte for en så stor og viktig tjeneste. 
async fn newClient(secretStore: &SecretStore, pool: &sqlx::PgPool) -> Client {
    println!("Logging in client!");

    let cookie_store = Arc::new(cookie::Jar::default());
    let client = Client::builder() // Den følge redirects by default
        .cookie_provider(cookie_store.clone())
        .build()
        .unwrap();

    // Først må vi redirectes fra login lenka te dataporten
    let res1 = client.get("https://tp.educloud.no/ntnu/?login=1")
        .send().await.unwrap();

    // Da kjem vi te en side der vi ska velg universitet. 
    // For å gjør det kan vi bare følg returnTo query parameter (etter å ha urldecoda det)
    // og hiv på authselection=feide|realm|ntnu.no
    let mut req2Url = String::from(res1.url().query().unwrap()
        .split("&").filter(|q|q.starts_with("returnTo")).next().unwrap()
        .split("=").nth(1).unwrap());

    req2Url = urlencoding::decode(&req2Url).unwrap().into_owned();

    req2Url.push_str("&authselection=feide|realm|ntnu.no");

    // Request login formet
    let res2 = client.get(req2Url).send().await.unwrap();

    // Log inn
    let res3 = client.post(res2.url().as_str()) // Postes te nøyaktig samme addresse
        .header("Content-Type", "application/x-www-form-urlencoded") // Må sett denne for at servern ska les form body
        .body(format!("has_js=0&feidename={}&password={}", 
            secretStore.get("FEIDE_BRUKERNAVN").unwrap(), 
            urlencoding::encode(&secretStore.get("FEIDE_PASSORD").unwrap()).into_owned())
        )
        .send().await.unwrap();

    // Plukk ut form data for å kunna generer request body
    let formAction;
    let SAMLResponse;
    let RelayState;

    { // Gjør funky scope greier her fordi ellers kan vi ikkje ha referansa te element, e scraper som e kjip
        let page = Html::parse_document(&res3.text().await.unwrap());

        let form = page.select(&Selector::parse("form").unwrap()).nth(0).unwrap();
        formAction = String::from(form.attr("action").unwrap());
        SAMLResponse = String::from(form.clone().select(&Selector::parse("input[name=\"SAMLResponse\"]").unwrap()).nth(0).unwrap().attr("value").unwrap());
        RelayState = String::from(form.clone().select(&Selector::parse("input[name=\"RelayState\"]").unwrap()).nth(0).unwrap().attr("value").unwrap());
    }

    let _res4 = client.post(formAction)
        .header("Content-Type", "application/x-www-form-urlencoded") // Må sett denne for at servern ska les body
        .body(format!("SAMLResponse={}&RelayState={}", 
            urlencoding::encode(&SAMLResponse).into_owned(), 
            urlencoding::encode(&RelayState).into_owned()
        ))
        .send().await.unwrap();

    pool.set(KEY_TP_COOKIE, cookie_store.clone().cookies(&Url::parse("https://tp.educloud.no").unwrap()).unwrap().to_str().unwrap()).await;

    println!("Finished logging in client!");
    client
}


/// Skaffe en reqwest::Clent med cookies fra postgres 
async fn getClient(pool: &sqlx::PgPool) -> Client {
    let cookieJar = Arc::new(cookie::Jar::default());
    cookieJar.add_cookie_str(&pool.get(KEY_TP_COOKIE).await, &"https://tp.educloud.no".parse::<reqwest::Url>().unwrap());
    Client::builder() // Den følge redirect by default
        .cookie_provider(cookieJar)
        .build()
        .unwrap()
}


// Hjelpefunksjon som skaffe en client og et sett bookings
// Dette fordi client validere cookie ved å send et request, og første request vi sende
// i begge inngangan (nettsida og cronjob) e å skaff egne bookings
async fn getClientAndBookings(secretStore: &SecretStore, pool: &sqlx::PgPool) -> (Client, Vec<Value>) {
    let client = getClient(pool).await;
    let bookings = client.getBookings().await;
    if bookings.is_ok() { // Om cookien e good
        return (client, bookings.unwrap().as_array().unwrap().to_owned());
    }

    // Om cookien ikkje e det
    let client = newClient(secretStore, pool).await;
    let bookings = client.getBookings().await.unwrap().as_array().unwrap().to_owned();
    (client, bookings)
}


// En for mazemap apien, en for sjølve mazemap lenka
const MAZEMAP_API_PREFIX: &str = "https://search.mazemap.com/search/equery/?rows=1&start=0&withpois=true&campusid=597&z=1&q=";
const MAZEMAP_PREFIX: &str = "https://use.mazemap.com/?utm_medium=shorturl&fromshortlink=true#v=1&campusid=21&sharepoitype=poi&sharepoi=";

/// Fordi vi ønske å unngå å send MazeMap 10 requests hver load tar vi heller å lenke til
/// en redirect lenke som redirecte til mazemap! Ganske fint system
#[get("/mazemap/<room>")]
async fn roomRedirect(room: &str) -> Redirect {
    let json: Value = Client::new().get(format!("{}{}", MAZEMAP_API_PREFIX, room))
        .send().await.unwrap().json().await.unwrap();

    let url = json.get("result").unwrap().get(0).unwrap().get("poiId").unwrap().as_i64().unwrap();

    Redirect::to(format!("{}{}", MAZEMAP_PREFIX, url.to_string()))
}


/// Hovedsida der man får en oversikt av bookingan
#[get("/")]
async fn index(
    secretStore: &State<SecretStore>,
    pool: &State<sqlx::PgPool>
) -> RawHtml<String> {
    let (_client, bookings) = getClientAndBookings(&secretStore, &pool).await;

    let bookings: Vec<Value> = bookings.into_iter().filter(|e| e.get("name").unwrap() == BOOKING_NAVN).collect();

    // Når du jobbe med dette kjør cargo watch -cqx 'shuttle run'
    // Når du jobbe med CSS kjør ./tailwindcss -o static/styles.css --watch --minify
    RawHtml(html!{
        (DOCTYPE)
        html {
            head {
                link rel="stylesheet" href="static/styles.css"
                meta name="viewport" content="width=device-width, initial-scale=1.0" {}
            }
            body class="text-center bg-[#aaf]" {
                div class="m-auto max-w-96" {
                    h1 class="text-2xl mt-4" { "HelgaSangern timeplan!" }
                    div class="flex flex-row justify-around h-12 pt-3 text-lg" {
                        span { "Rom" }
                        span { "Dato" }
                        span { "Klokkeslett" }
                    }
                    @for booking in &bookings { 
                        div class="flex flex-row justify-around h-12 pt-3" {
                            a href=(uri!(roomRedirect(booking.get("rooms").unwrap().get(0).unwrap().get("name").unwrap().as_str().unwrap()))) 
                                { (booking.get("rooms").unwrap().get(0).unwrap().get("name").unwrap().as_str().unwrap()) }
                            @let bookingDay = NaiveDate::parse_from_str(booking.get("firstday").unwrap().as_str().unwrap(), "%F").unwrap();
                            span { (bookingDay.format_localized("%A den %e.", Locale::nb_NO)) }
                            span { (booking.get("tid").unwrap().as_str().unwrap()) }
                        }
                    }
                    div { "
Denne nettsiden bruke Jakob sin rombooking til å automatisk booke rom på Helgasetr til TXS kollokvie.
Alle rom-navnene lenker til mazemap:)
                    " }
                }
            }
        }
    }.into_string())
}


#[shuttle_runtime::main]
async fn main(
    #[shuttle_runtime::Secrets] secretStore: SecretStore,
    #[shuttle_shared_db::Postgres(
        local_uri = "postgres://postgres:postgrespassword@localhost:5432/helgasangerntest"
    )] pool: sqlx::PgPool,
) -> ShuttleRocket {
    sqlx::migrate!().run(&pool).await.expect("Migrations failed :( ");

    let secreteStoreClone = secretStore.clone();
    let poolClone = pool.clone();

    // lmao, dette va my enklar enn det vi gjor på tracking helper tidligar haha
    // TODO: E veit ikkje om denne måten å hånter state på fungere, men det virke nå sånn?
    tokio::spawn(async move {
        let mut interval = interval(TokioDuration::from_secs(60 * 60));
        loop {
            interval.tick().await;

            // Your cron job logic here
            println!("Running cron job");

            bookRooms(&secreteStoreClone, &poolClone).await;
        }
    });

    let rocket = rocket::build()
        .mount("/static", FileServer::from(relative!("static/")))
        .mount("/", routes![index, roomRedirect])
        .manage(secretStore)
        .manage(pool);

    Ok( rocket.into() )
}
