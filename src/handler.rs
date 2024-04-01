use std::{collections::HashMap, fmt::Debug, hash::Hash, time::Duration};

use anyhow::Result;
use axum::{extract::Path, http::StatusCode, Json};
use futures::future::join_all;
use itertools::Itertools;
use reqwest::{Client, Url};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thirtyfour::{cookie::SameSite, prelude::*};
use tokio::time::sleep;

use crate::{config::CONFIG, errors::AppError};

pub async fn health_check() -> (StatusCode, String) {
    let health = true;
    match health {
        true => (StatusCode::OK, "Healthy!".to_string()),
        false => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Not healthy!".to_string(),
        ),
    }
}

async fn get_chrome_driver() -> Result<WebDriver, WebDriverError> {
    let mut caps = DesiredCapabilities::chrome();
    caps.set_ignore_certificate_errors()?;
    caps.add_chrome_arg("--disable-dev-tools")?;
    caps.add_chrome_arg("--user-data-dir=/tmp/user-data")?;
    #[cfg(any(feature = "lambda", feature = "ecs", feature = "headless"))]
    {
        caps.set_disable_dev_shm_usage()?;
        caps.set_disable_gpu()?;
        caps.set_disable_web_security()?;
        caps.set_headless()?;
        caps.set_no_sandbox()?;
        caps.add_chrome_arg("--no-zygote")?;
        caps.add_chrome_arg("--single-process")?;
    }
    let driver = WebDriver::new("http://localhost:9515", caps).await;
    driver
}

pub async fn test_handler() -> ApiResponse<Value> {
    tryhard::retry_fn(|| async {
        let driver = get_chrome_driver().await?;
        driver.goto("https://example.com").await?;
        let title = driver.title().await?;
        driver.quit().await?;

        Ok((StatusCode::OK, Json(json!({ "title": title }))))
    })
    .retries(10)
    .max_delay(Duration::from_secs(10))
    .exponential_backoff(Duration::from_secs(1))
    .await
}

#[derive(Deserialize)]
pub struct RequestBusinessProfileReportParams {
    pub search_business_params: SearchBusinessRegistryParams,
    pub selected_company: String,
    pub search_product: String,
    #[serde(default = "default_email")]
    pub email: String,
}

fn default_email() -> String {
    CONFIG.default_email.clone()
}

#[derive(Deserialize, Serialize, Debug)]
pub enum StatusKey {
    Active,
    Inactive,
    #[serde(rename(serialize = "-- All Statuses --"))]
    All,
}

#[derive(Deserialize, Serialize, Debug, Hash, Eq, PartialEq)]
pub enum RegisterType {
    #[serde(rename(serialize = "-- All Registers --"))]
    All,
    Corporations,
    #[serde(rename = "Business Names")]
    BusinessNames,
    Partnerships,
}

#[derive(Deserialize, Serialize, Eq, PartialEq)]
pub enum SearchOperator {
    On,
    Before,
    #[serde(rename = "From or On")]
    FromOrOn,
    Between,
}

#[derive(Deserialize, Default, Clone, Debug)]
#[serde(try_from = "String")]
pub struct DateInput(String);
impl TryFrom<String> for DateInput {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        // Month Day, Year
        let re = regex::Regex::new(r"^([A-Z][a-z]+) (\d{1,2}), (\d{4})$").unwrap();
        if re.is_match(&value) {
            Ok(DateInput(value))
        } else {
            Err("Invalid date format, must be 'Month Day, Year' e.g. 'January 1, 2021'".to_string())
        }
    }
}
impl AsRef<str> for DateInput {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Deserialize)]
#[serde(try_from = "SearchBusinessRegistryParamsShadow")]
pub struct SearchBusinessRegistryParams {
    pub query_word: String,
    pub register_type_key: Option<RegisterType>,
    pub business_type_selection: Option<String>,
    pub status_key: Option<StatusKey>,
    pub date_input: Option<DateInput>,
    pub search_operator: Option<SearchOperator>,
    pub end_date: Option<DateInput>,
}

#[derive(Deserialize)]
pub struct SearchBusinessRegistryParamsShadow {
    pub query_word: String,
    pub register_type_key: Option<RegisterType>,
    pub business_type_selection: Option<String>,
    pub status_key: Option<StatusKey>,
    pub date_input: Option<DateInput>,
    pub search_operator: Option<SearchOperator>,
    pub end_date: Option<DateInput>,
}

impl TryFrom<SearchBusinessRegistryParamsShadow> for SearchBusinessRegistryParams {
    type Error = String;

    fn try_from(value: SearchBusinessRegistryParamsShadow) -> Result<Self, Self::Error> {
        let mut register_to_business_type_map: HashMap<RegisterType, Vec<&str>> = HashMap::new();
        register_to_business_type_map.insert(
            RegisterType::Corporations,
            vec![
                "-- Any type --",
                // redacted
            ],
        );

        register_to_business_type_map.insert(
            RegisterType::BusinessNames,
            vec![
                "-- Any type --",
                // redacted
            ],
        );

        register_to_business_type_map.insert(
            RegisterType::Partnerships,
            vec![
                "-- Any type --",
                // redacted
            ],
        );

        match value.register_type_key {
            Some(RegisterType::All) | None => {}
            Some(ref register_type) => {
                if let Some(business_types) = register_to_business_type_map.get(&register_type) {
                    if let Some(business_type) = value.business_type_selection.as_deref() {
                        if business_type != "-- Any type --"
                            && !business_types.contains(&business_type)
                        {
                            return Err(format!(
                                "Invalid business type for register type '{}', must be one of {}",
                                serde_json::to_string(&register_type).unwrap(),
                                business_types.join(", ")
                            ));
                        }
                    };
                }
            }
        };

        Ok(SearchBusinessRegistryParams {
            query_word: value.query_word,
            register_type_key: value.register_type_key,
            business_type_selection: value.business_type_selection,
            status_key: value.status_key,
            date_input: value.date_input,
            search_operator: value.search_operator,
            end_date: value.end_date,
        })
    }
}

async fn goto_payment_page(
    driver: &WebDriver,
    param: &RequestBusinessProfileReportParams,
) -> WebDriverResult<()> {
    let RequestBusinessProfileReportParams {
        selected_company,
        search_product,
        email,
        ..
    } = param;
    let search_element = driver
        .query(By::XPath(&format!(
            "//span[contains(text(), '{}')]",
            selected_company
        )))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    search_element.click().await?;

    // page3
    let search_element = driver
        .query(By::XPath(
            "//span[contains(text(), 'Request Search Products')]",
        ))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    search_element.click().await?;

    // page4
    // from here profile report is getting started
    let radio_button = driver
        .query(By::XPath("//label[contains(text(), 'from the Ministry')]"))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    radio_button.click().await?;

    let radio_button = driver
        .query(By::XPath(&format!(
            "//label[contains(text(), '{}')]",
            search_product
        )))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    radio_button.click().await?;

    let search_element = driver
        .query(By::XPath("//span[contains(text(), 'Continue')]"))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    search_element.click().await?;

    // page5
    // option1
    if search_product == "Profile Report" {
        let radio_button = driver
            .query(By::XPath("//label[contains(text(), 'Current Report')]"))
            .wait(Duration::from_secs(20), Duration::from_secs(1))
            .first()
            .await?;
        radio_button.click().await?;
        // sleep 5 seconds
        sleep(Duration::from_secs(5)).await;

        let email_inputs = driver
            .query(By::XPath("//input[@type='email']"))
            .wait(Duration::from_secs(10), Duration::from_secs(1))
            .all()
            .await?;
        for email_input in email_inputs {
            email_input.send_keys(email).await?;
        }

        let submit_element = driver
            .query(By::XPath("//span[contains(text(), 'Submit')]"))
            .wait(Duration::from_secs(20), Duration::from_secs(1))
            .first()
            .await?;
        submit_element.click().await?;
    }
    // option2
    if search_product == "Document Copies" {
        let check_box = driver
            .query(By::XPath(
                "//label[contains(text(), 'Select all Documents')]",
            ))
            .wait(Duration::from_secs(20), Duration::from_secs(1))
            .first()
            .await?;
        check_box.click().await?;
        // sleep 5 seconds
        sleep(Duration::from_secs(5)).await;

        let email_inputs = driver
            .query(By::XPath("//input[@type='email']"))
            .wait(Duration::from_secs(10), Duration::from_secs(1))
            .all()
            .await?;
        for email_input in email_inputs {
            email_input.send_keys(email).await?;
        }

        let submit_element = driver
            .query(By::XPath("//span[contains(text(), 'Request Documents')]"))
            .wait(Duration::from_secs(20), Duration::from_secs(1))
            .first()
            .await?;
        submit_element.click().await?;
    }
    // option 3
    if search_product == "Certificate of Status" {
        println!("Certificate of Status is excuted");
        let email_inputs = driver
            .query(By::XPath("//input[@type='email']"))
            .wait(Duration::from_secs(10), Duration::from_secs(1))
            .all()
            .await?;
        for email_input in email_inputs {
            email_input.send_keys(email).await?;
        }

        let submit_element = driver
            .query(By::XPath("//span[contains(text(), 'Submit')]"))
            .wait(Duration::from_secs(20), Duration::from_secs(1))
            .first()
            .await?;
        submit_element.click().await?;
    }
    // page6
    let credit_dropdown = driver
        .query(By::XPath("//option[contains(text(), 'Credit Card')]"))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    credit_dropdown.click().await?;

    // sleep 5 seconds
    sleep(Duration::from_secs(5)).await;

    let submit_element = driver
        .query(By::XPath(
            "(//div[@class='appBoxChildren appBlockChildren'])[last()]/button[1]",
        ))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    submit_element.click().await?;
    // page7
    let make_payment = driver
        .query(By::XPath("//button[@id='submit_btn']"))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;

    make_payment.click().await?;
    sleep(Duration::from_secs(5)).await;

    let trn_card_owner = driver
        .query(By::XPath("//input[@name='trnCardOwner']"))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    trn_card_owner.send_keys(&CONFIG.card_name).await?;
    let trn_card_number = driver
        .query(By::XPath("//input[@name='trnCardNumber']"))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    trn_card_number.send_keys(&CONFIG.card_number).await?;
    let trn_exp_month = driver
        .query(By::XPath("//input[@id='trnExpMonth']"))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    trn_exp_month.send_keys(&CONFIG.card_month).await?;
    let trn_exp_year = driver
        .query(By::XPath("//input[@id='trnExpYear']"))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    trn_exp_year.send_keys(&CONFIG.card_year).await?;
    let trn_card_cvd = driver
        .query(By::XPath("//input[@name='trnCardCvd']"))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    trn_card_cvd.send_keys(&CONFIG.card_cvv).await?;
    let submit_payment = driver
        .query(By::XPath("//button[@id='submitButton']"))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    submit_payment.click().await?;

    Ok(())
}

async fn goto_search_result_page(
    driver: &WebDriver,
    params: &SearchBusinessRegistryParams,
) -> WebDriverResult<Option<Url>> {
    let SearchBusinessRegistryParams {
        query_word,
        register_type_key,
        business_type_selection,
        status_key,
        date_input,
        search_operator,
        end_date,
        ..
    } = params;

    // driver
    //     .goto("redacted")
    //     .await?;
    // println!("Current URL: {}", driver.current_url().await?.to_string());

    // // page1
    // let search_element = driver
    //     .query(By::XPath(
    //         "//a[contains(text(), 'redacted')]",
    //     ))
    //     .wait(Duration::from_secs(20), Duration::from_secs(1))
    //     .first()
    //     .await?;
    // search_element.click().await?;

    driver.goto("redacted").await?;
    let mut headers: HashMap<&str, &str> = HashMap::new();
    headers.insert("x-catalyst-timezone", "America/Toronto");

    for (key, value) in headers {
        let mut cookie = Cookie::new(key, value);
        cookie.set_domain("redacted");
        cookie.set_path("/");
        cookie.set_same_site(Some(SameSite::Lax));
        driver.add_cookie(cookie).await?;
    }

    println!("Current URL: {}", driver.current_url().await?.to_string());

    // page2
    let searchquery_element = driver
        .query(By::XPath("//input[@name='QueryString']"))
        .wait(Duration::from_secs(160), Duration::from_secs(1))
        .first()
        .await?;
    searchquery_element.send_keys(query_word).await?;

    let advanced_button = driver
        .query(By::XPath("//a[@aria-label=' Advanced']"))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    advanced_button.click().await?;

    let register_select = driver
        .query(By::XPath(&format!(
            "//option[contains(text(), '{}')]",
            serde_json::to_string(&register_type_key)
                .unwrap()
                .trim_matches('"')
        )))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    register_select.click().await?;
    sleep(Duration::from_secs(2)).await;

    if let Some(business_type_selection) = business_type_selection {
        let business_type_select = driver
            .query(By::XPath(&format!(
                "//option[contains(text(), '{}')]",
                business_type_selection
            )))
            .wait(Duration::from_secs(20), Duration::from_secs(1))
            .first()
            .await?;
        business_type_select.click().await?;
    }

    if let Some(status_key) = status_key {
        let status_select = driver
            .query(By::XPath(&format!(
                "//option[contains(text(), '{}')]",
                serde_json::to_string(&status_key)
                    .unwrap()
                    .trim_matches('"')
            )))
            .wait(Duration::from_secs(20), Duration::from_secs(1))
            .first()
            .await?;
        status_select.click().await?;
    }

    if let Some(date_input) = date_input {
        let registered_date_field = driver
            .query(By::XPath("//input[@name='RegistrationDate']"))
            .wait(Duration::from_secs(20), Duration::from_secs(1))
            .first()
            .await?;
        registered_date_field.send_keys(date_input).await?;
    }

    if let Some(search_operator) = search_operator {
        let search_operator_select = driver
            .query(By::XPath(&format!(
                "//option[contains(text(), '{}')]",
                serde_json::to_string(&search_operator)
                    .unwrap()
                    .trim_matches('"')
            )))
            .wait(Duration::from_secs(20), Duration::from_secs(1))
            .first()
            .await?;
        search_operator_select.click().await?;
    }

    if let Some(SearchOperator::Between) = search_operator {
        sleep(Duration::from_secs(2)).await;
        let end_date_input = driver
            .query(By::XPath("//input[@name='RegistrationDate2']"))
            .wait(Duration::from_secs(20), Duration::from_secs(1))
            .first()
            .await?;
        end_date_input
            .send_keys(end_date.clone().unwrap_or_default())
            .await?;
        end_date_input.send_keys("" + Key::Enter).await?;
    }

    let searchbutton_element = driver
        .query(By::XPath(
            "//div[@class='appBox appBlock registerItemSearch-tabs-criteriaAndButtons-buttonPad \
             appButtonPad appSearchButtonPad appNotReadOnly appIndex1 appChildCount3']/div/button",
        ))
        .wait(Duration::from_secs(20), Duration::from_secs(1))
        .first()
        .await?;
    searchbutton_element.click().await?;

    sleep(Duration::from_secs(5)).await;

    // check if #appSearchNoResults exists
    if let Ok(_) = driver
        .query(By::XPath("//div[@id='appSearchNoResults']"))
        .wait(Duration::from_secs(5), Duration::from_secs(1))
        .first()
        .await
    {
        println!("No results found");
        return Ok(None);
    }

    let page_size_selector = driver
        .query(By::XPath(&format!(
            "//div[@class='appSearchPageSize']/select/option[contains(text(), '{}')]",
            200
        )))
        .first()
        .await?;
    page_size_selector.click().await?;
    sleep(Duration::from_secs(15)).await;

    let current_url = driver.current_url().await?;

    Ok(Some(current_url))
}

pub async fn get_payment_page_handler(
    Json(params): Json<RequestBusinessProfileReportParams>,
) -> ApiResponse<Value> {
    tryhard::retry_fn(|| async {
        let driver = get_chrome_driver().await?;

        if goto_search_result_page(&driver, &params.search_business_params)
            .await?
            .is_none()
        {
            return Ok((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "No results found" })),
            ));
        }
        goto_payment_page(&driver, &params).await?;

        let dcurrent_url = driver.current_url().await?;

        let result_json = json!({
            "current_url": dcurrent_url.to_string(),
        });

        driver.quit().await?;

        Ok((StatusCode::OK, Json(result_json)))
    })
    .retries(10)
    .max_delay(Duration::from_secs(10))
    .exponential_backoff(Duration::from_secs(1))
    .await
}

pub async fn get_companies_list_handler(
    Json(params): Json<SearchBusinessRegistryParams>,
) -> ApiResponse<Value> {
    tryhard::retry_fn(|| async {
        let driver = get_chrome_driver().await?;

        if goto_search_result_page(&driver, &params).await?.is_none() {
            return Ok((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "No results found" })),
            ));
        }

        let company_links = driver
            .query(By::XPath(
                "//a[@class='registerItemSearch-results-page-line-ItemBox-resultLeft-viewMenu \
                 appMenu appMenuItem appMenuDepth0 appItemSearchResult noSave \
                 viewInstanceUpdateStackPush appReadOnly appIndex0']",
            ))
            .all()
            .await?;
        let company_names: Vec<String> = join_all(company_links.iter().map(|link| link.text()))
            .await
            .into_iter()
            .map(|x| x.unwrap())
            .collect();

        let current_url = driver.current_url().await?;

        let result_json = json!({
            "company_names": company_names,
            "current_url": current_url.to_string(),
        });

        driver.quit().await?;

        Ok((StatusCode::OK, Json(result_json)))
    })
    .retries(10)
    .max_delay(Duration::from_secs(10))
    .exponential_backoff(Duration::from_secs(1))
    .await
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Either<L, R> {
    Left(L),
    Right(R),
}

#[derive(Serialize, Deserialize, Debug)]
struct Scrap {
    corporate_number: String,
    first_name: String,
    last_name: String,
    phone_number: String,
    #[serde(default = "default_email")]
    email: String,
    summarize_data: Vec<Value>,
    contact: String,
    url: String,
}

impl Scrap {
    async fn create_request(&self, client: &Client) -> Result<(), reqwest::Error> {
        let url_contacts = format!("{}/cntcts", self.url);
        let payload_contacts = serde_json::json!({
            "contactMethod": {
                "phoneNumber": self.phone_number,
                "emailAddress": self.email
            },
            "firstName": self.first_name,
            "lastName": self.last_name
        });

        let response_contacts = client
            .post(&url_contacts)
            .json(&payload_contacts)
            .send()
            .await?;

        println!("Status Code Contacts: {}", response_contacts.status());
        println!(
            "Response Content Contacts: {:?}",
            response_contacts.text().await?
        );
        Ok(())
    }

    async fn get_request(&mut self, client: &Client) -> Result<(), reqwest::Error> {
        let url_contacts_query = format!("{}/cntcts", self.url);
        let params_contacts_query = [
            ("eaddr", &self.email),
            ("frstNm", &self.first_name),
            ("lstNm", &self.last_name),
            ("phnn", &self.phone_number),
        ];

        let response_contacts_query = client
            .get(&url_contacts_query)
            .query(&params_contacts_query)
            .send()
            .await?;

        println!(
            "Status Code Contacts Query: {}",
            response_contacts_query.status()
        );
        let response_text = response_contacts_query.text().await?;
        self.contact = serde_json::from_str::<Value>(&response_text)
            .unwrap()
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        println!("Response Content Contacts Query: {}", response_text);
        Ok(())
    }

    fn data_parser(&mut self, data: Vec<Value>) {
        self.summarize_data = data
            .into_iter()
            .map(|mut item| {
                item.as_object_mut()
                    .map(|obj| {
                        obj.remove("sourceRequest");
                        obj.remove("documentType");
                    })
                    .unwrap_or_default();
                item
            })
            .collect();
    }

    async fn summary_data(&mut self, client: &Client) -> Result<(), reqwest::Error> {
        let url = format!("{}/dcmnts?crprtnid={}", self.url, self.corporate_number);
        let response = client.get(&url).send().await?;

        if response.status().is_success() {
            let json_data = response.json::<Vec<Value>>().await?;
            self.data_parser(json_data);
        } else {
            println!("Request failed with status code: {}", response.status());
        }
        Ok(())
    }

    async fn extract_data(
        corporate_name: &str,
        num_of_records: Option<usize>,
    ) -> Result<Vec<HashMap<String, String>>, reqwest::Error> {
        let mut data: Vec<HashMap<String, String>> = Vec::new();
        let mut page_number = 0;
        let mut next_page = true;

        while next_page && data.len() < num_of_records.unwrap_or(usize::MAX) {
            println!("extracting page {}", page_number);
            let url = format!("https://redacted/cc/lgcy/fdrlCrpSrch.html?p={}&crpNm={}&crpNmbr=&bsNmbr=&cProv=&cStatus=&cAct=", page_number, corporate_name);
            let response = reqwest::get(&url).await?;
            let html = response.text().await?;

            let document = Html::parse_document(&html);

            let rows_selector = Selector::parse("div.col-md-11").unwrap();
            let rows = document.select(&rows_selector);

            for row in rows {
                let row_spans = row
                    .select(&Selector::parse("span").unwrap())
                    .collect::<Vec<_>>();
                let business_name = row_spans[0]
                    .select(&Selector::parse("a").unwrap())
                    .next()
                    .unwrap()
                    .inner_html();
                let status = row_spans[1].inner_html();
                let status = status.split(':').nth(1).unwrap().trim();
                let corporation_number = row_spans[2].inner_html();
                let corporation_number = corporation_number.split(':').nth(1).unwrap().trim();
                let business_number = row_spans[3].inner_html();
                let business_number = business_number.split(':').nth(1).unwrap().trim();

                let mut row_data: HashMap<String, String> = HashMap::new();
                row_data.insert("business_name".to_string(), business_name);
                row_data.insert("status".to_string(), status.to_string());
                row_data.insert(
                    "corporation_number".to_string(),
                    corporation_number.replace('-', ""),
                );
                row_data.insert("business_number".to_string(), business_number.to_string());

                data.push(row_data);
            }

            if document
                .select(&Selector::parse("a[rel=\"next\"]").unwrap())
                .next()
                .is_none()
            {
                next_page = false;
            }

            page_number += 1;
        }

        Ok(data)
    }

    async fn table_pass(&self, client: &Client) -> Result<(), reqwest::Error> {
        let url = format!("{}/rqsts", self.url);
        let payload = serde_json::json!({
            "@type": "copies",
            "corporation": self.corporate_number,
            "summaries": self.summarize_data,
            "contact": self.contact
        });

        let response = client.post(&url).json(&payload).send().await?;

        println!("Status Code: {}", response.status());
        println!("Response Content: {:?}", response.text().await?);
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CorporationDataExtract {
    url: String,
}

impl CorporationDataExtract {
    fn gen_url(corporation_id: String) -> String {
        format!(
            "https://redacted/cc/lgcy/fdrlCrpDtls.html?p=0&corpId={corporation_id}&V_TOKEN=null&crpNm=Tech&crpNmbr=&bsNmbr=&cProv=&cStatus=&cAct=",
            corporation_id = corporation_id
        )
    }

    fn extract_corp_details(html_data: &Html) -> Vec<HashMap<String, String>> {
        let rows = html_data
            .select(&Selector::parse("div.col-sm-12").unwrap())
            .nth(2)
            .unwrap();
        let rows = rows
            .select(&Selector::parse("div.data-display-group").unwrap())
            .collect_vec();
        let mut data: Vec<HashMap<String, String>> = Vec::new();

        for row in rows {
            let key = row
                .select(&Selector::parse("b").unwrap())
                .next()
                .unwrap()
                .inner_html();

            let value = if key == "Corporate Name" {
                row.select(&Selector::parse("div.col-sm-8").unwrap())
                    .next()
                    .unwrap()
                    .text()
                    .map(|s| s.trim().to_string())
                    .join("")
                    .split("<br>")
                    .next()
                    .unwrap()
                    .to_string()
            } else {
                row.select(&Selector::parse("div.col-sm-8").unwrap())
                    .next()
                    .unwrap()
                    .text()
                    .map(|s| s.trim().to_string())
                    .join("")
                    .to_string()
            };

            let mut row_data: HashMap<String, String> = HashMap::new();
            row_data.insert(key.trim().to_string(), value.trim().to_string());
            data.push(row_data);
        }

        data
    }

    fn extract_address_details(html_data: &Html) -> String {
        let html_data = html_data
            .select(&Selector::parse("div.col-sm-12").unwrap())
            .nth(3)
            .unwrap();
        let address = html_data
            .select(&Selector::parse("div").unwrap())
            .next()
            .unwrap()
            .text()
            .collect_vec();

        address
            .iter()
            .filter_map(|s| {
                let s = s.trim();
                if s.is_empty() {
                    None
                } else {
                    Some(s.to_string())
                }
            })
            .join(", ")
    }

    fn extract_director_details(html_data: &Html) -> HashMap<String, Vec<HashMap<String, String>>> {
        let html_data = html_data
            .select(&Selector::parse("div.col-sm-12").unwrap())
            .nth(5)
            .unwrap();

        let director_count = html_data
            .select(&Selector::parse("div.inline-group").unwrap())
            .next()
            .unwrap();
        let mut director_count_data: Vec<HashMap<String, String>> = Vec::new();
        for row in director_count.select(&Selector::parse("div").unwrap()) {
            if let Some(key) = row.select(&Selector::parse("b").unwrap()).next() {
                let value = row
                    .select(&Selector::parse("span").unwrap())
                    .next()
                    .unwrap()
                    .inner_html();
                let mut row_data: HashMap<String, String> = HashMap::new();
                row_data.insert(
                    key.inner_html().trim().to_string(),
                    value.trim().to_string(),
                );
                director_count_data.push(row_data);
            }
        }

        let directors_lists = html_data
            .select(&Selector::parse("li.full-width").unwrap())
            .collect_vec();

        let mut directors_personal_data: Vec<HashMap<String, String>> = Vec::new();

        for row in directors_lists {
            let director_p = row.text().map(|s| s.trim().to_string()).collect_vec();
            let name = director_p[0].to_string();
            let address = director_p[1..].join(", ");
            let mut row_data: HashMap<String, String> = HashMap::new();
            row_data.insert("name".to_string(), name);
            row_data.insert("address".to_string(), address);
            directors_personal_data.push(row_data);
        }

        let mut directors_final_data: HashMap<String, Vec<HashMap<String, String>>> =
            HashMap::new();
        directors_final_data.insert("director_count".to_string(), director_count_data.to_vec());
        directors_final_data.insert(
            "director_personal_data".to_string(),
            directors_personal_data.to_vec(),
        );

        directors_final_data
    }

    fn extract_annual_filings_details(html_data: &Html) -> AnnualFilingDetails {
        let rows = html_data
            .select(&Selector::parse("div.col-sm-12").unwrap())
            .nth(7)
            .unwrap();
        let rows = rows
            .select(&Selector::parse("div.data-display-group").unwrap())
            .collect_vec();
        let mut data: AnnualFilingDetails = Vec::new();

        for row in rows {
            let key = row
                .select(&Selector::parse("b").unwrap())
                .next()
                .unwrap()
                .text()
                .map(|s| s.trim().to_string())
                .join("");

            let value = if key != "Status of Annual Filings" {
                let value = row
                    .select(&Selector::parse("div.col-sm-9").unwrap())
                    .next()
                    .unwrap()
                    .text()
                    .map(|s| s.split(' ').map(|s| s.trim()).join(" "))
                    .join("")
                    .trim()
                    .to_string();
                Either::Left(value)
            } else {
                let status_div = row
                    .select(&Selector::parse("div.col-sm-9").unwrap())
                    .next()
                    .unwrap();
                let list_elements = status_div
                    .select(&Selector::parse("li").unwrap())
                    .collect_vec();
                let value = list_elements
                    .iter()
                    .map(|l| {
                        let text = l.text().map(|s| s.trim().to_string()).join("");
                        let text = text.split('-').collect_vec();
                        let key = text[0].to_string();
                        let value = text[1].to_string();
                        let mut row_data: HashMap<String, String> = HashMap::new();
                        row_data.insert(key, value);
                        row_data
                    })
                    .collect_vec();
                Either::Right(value)
            };

            let mut row_data: HashMap<String, Either<String, Vec<HashMap<String, String>>>> =
                HashMap::new();
            row_data.insert(key.trim().to_string(), value);
            data.push(row_data);
        }

        data
    }

    fn extract_corp_history_details(
        html_data: &Html,
    ) -> HashMap<String, Vec<HashMap<String, String>>> {
        let html_data = html_data
            .select(&Selector::parse("div.col-sm-12").unwrap())
            .nth(8)
            .unwrap();

        let table_data = html_data
            .select(&Selector::parse("table").unwrap())
            .next()
            .unwrap();
        let heading = table_data
            .select(&Selector::parse("thead").unwrap())
            .next()
            .unwrap()
            .text()
            .map(|s| s.trim().to_string())
            .join("");
        let td_data = table_data
            .select(&Selector::parse("td").unwrap())
            .collect_vec();
        let table_info = td_data
            .iter()
            .map(|data| {
                let row_val = data
                    .text()
                    .flat_map(|s| s.split(' ').map(|s| s.trim()))
                    .filter(|s| !s.is_empty())
                    .collect_vec()
                    .join(" ");
                row_val
            })
            .collect_vec();

        let name_history_data = table_info
            .chunks(2)
            .map(|data| {
                let key = data[0].to_string();
                let value = data[1].to_string();
                let mut row_data: HashMap<String, String> = HashMap::new();
                row_data.insert(key, value);
                row_data
            })
            .collect_vec();

        let section = html_data
            .select(&Selector::parse("section.panel-info").unwrap())
            .next()
            .unwrap();
        let section_header = section
            .select(&Selector::parse("header").unwrap())
            .next()
            .unwrap()
            .text()
            .map(|s| s.trim().to_string())
            .join("");

        let panel_body = section
            .select(&Selector::parse("div.panel-body").unwrap())
            .next()
            .unwrap();

        let rows = panel_body
            .select(&Selector::parse("div.data-display-group").unwrap())
            .collect_vec();
        let mut panel_data: Vec<HashMap<String, String>> = Vec::new();
        for row in rows {
            let key = row
                .select(&Selector::parse("b").unwrap())
                .next()
                .unwrap()
                .text()
                .map(|s| s.trim().to_string())
                .join("");
            let value = row
                .select(&Selector::parse("div.col-sm-6").unwrap())
                .next()
                .unwrap()
                .text()
                .map(|s| s.trim().to_string())
                .join("");
            let mut row_data: HashMap<String, String> = HashMap::new();
            row_data.insert(key.trim().to_string(), value.trim().to_string());
            panel_data.push(row_data);
        }

        let mut data: HashMap<String, Vec<HashMap<String, String>>> = HashMap::new();
        data.insert(heading, name_history_data);
        data.insert(section_header, panel_data);

        data
    }

    async fn extract_corporation_data(url: String) -> ApiResponse<CorporationData> {
        let response = reqwest::get(&url).await.unwrap();
        let html = response.text().await.unwrap();
        let document = Html::parse_document(&html);

        let corp_details = CorporationDataExtract::extract_corp_details(&document);
        let address_details = CorporationDataExtract::extract_address_details(&document);
        let director_details = CorporationDataExtract::extract_director_details(&document);
        let annual_filings_details =
            CorporationDataExtract::extract_annual_filings_details(&document);
        let corp_history_details = CorporationDataExtract::extract_corp_history_details(&document);

        let data = CorporationData {
            corp_details,
            address_details,
            director_details,
            annual_filings_details,
            corp_history_details,
        };

        Ok((StatusCode::OK, Json(data)))
    }
}

type AnnualFilingDetails = Vec<HashMap<String, Either<String, Vec<HashMap<String, String>>>>>;

#[derive(Debug, Serialize, Deserialize)]
pub struct CorporationData {
    corp_details: Vec<HashMap<String, String>>,
    address_details: String,
    director_details: HashMap<String, Vec<HashMap<String, String>>>,
    annual_filings_details: AnnualFilingDetails,
    corp_history_details: HashMap<String, Vec<HashMap<String, String>>>,
}

pub async fn corporation_get(Path(id): Path<String>) -> ApiResponse<CorporationData> {
    CorporationDataExtract::extract_corporation_data(CorporationDataExtract::gen_url(id)).await
}

pub async fn registries_get(
    Path(search_keyword): Path<String>,
) -> ApiResponse<Vec<HashMap<String, String>>> {
    let data = Scrap::extract_data(&search_keyword, None).await?;
    Ok((StatusCode::OK, Json(data)))
}

#[derive(Deserialize)]
pub struct RegistryRequest {
    corporate_number: String,
    first_name: String,
    last_name: String,
    phone_number: String,
    #[serde(default = "default_email")]
    email: String,
}

async fn request_registry(
    client: Client,
    corporate_number: String,
    first_name: String,
    last_name: String,
    phone_number: String,
    email: String,
) -> Result<(), reqwest::Error> {
    let mut scrap = Scrap {
        corporate_number,
        first_name,
        last_name,
        phone_number,
        email,
        summarize_data: vec![],
        contact: String::new(),
        url: "https://redacted/cc/api".to_string(),
    };

    scrap.create_request(&client).await?;
    scrap.get_request(&client).await?;
    scrap.summary_data(&client).await?;
    scrap.table_pass(&client).await?;

    Ok(())
}

pub async fn registry_request(Json(request): Json<RegistryRequest>) -> ApiResponse<Value> {
    let client = Client::new();

    let RegistryRequest {
        corporate_number,
        first_name,
        last_name,
        phone_number,
        email,
    } = request;

    request_registry(
        client.clone(),
        corporate_number,
        first_name,
        last_name,
        phone_number,
        email,
    )
    .await?;

    Ok((StatusCode::OK, Json(json!("success"))))
}

#[derive(Deserialize)]
pub struct RegistryRequestByName {
    search_keyword: String,
    first_name: String,
    last_name: String,
    phone_number: String,
    #[serde(default = "default_email")]
    email: String,
}

pub async fn registry_request_by_name(
    Json(request): Json<RegistryRequestByName>,
) -> ApiResponse<Value> {
    let client = Client::new();

    let RegistryRequestByName {
        search_keyword,
        first_name,
        last_name,
        phone_number,
        email,
    } = request;

    let data = Scrap::extract_data(&search_keyword, Some(1)).await?;
    let corporate_number = data[0].get("corporation_number").unwrap().to_string();

    request_registry(
        client.clone(),
        corporate_number,
        first_name,
        last_name,
        phone_number,
        email,
    )
    .await?;

    Ok((StatusCode::OK, Json(json!("success"))))
}

type ApiResponse<T> = Result<(StatusCode, Json<T>), AppError>;
